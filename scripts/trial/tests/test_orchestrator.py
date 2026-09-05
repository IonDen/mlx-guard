"""Orchestrator tests. Each docstring names the one-line bug that turns the test red.

The fake binary is a real ``/bin/sh`` script (never mlx-guard itself) that writes a minimal
report to whatever path follows ``--report``, controlled by two environment variables so a
single script can stand in for both a passing and a failing arm.
"""

import json
from pathlib import Path

import bands
import orchestrator
import pytest
import validators as v

FAKE_BINARY = """#!/bin/sh
prev=""
for arg in "$@"; do
    if [ "$prev" = "--report" ]; then
        printf '%s' "$FAKE_REPORT_BODY" > "$arg"
    fi
    prev="$arg"
done
exit "${FAKE_EXIT_CODE:-0}"
"""

PREDICTION_CHECKING_BINARY = """#!/bin/sh
prev=""
report=""
for arg in "$@"; do
    if [ "$prev" = "--report" ]; then
        report="$arg"
    fi
    prev="$arg"
done
if [ -f ../prediction.json ]; then
    code=0
else
    code=99
fi
printf '{"outcome": {"kind": "child_exited", "code": '"$code"'}, "signals": [], \
"checkpoint": {"status": "not_negotiated"}, "transitions": [], "samples": []}' > "$report"
exit 0
"""

UNEVENTFUL_REPORT = """{
  "outcome": {"kind": "child_exited", "code": 0},
  "signals": [],
  "checkpoint": {"status": "not_negotiated"},
  "transitions": [],
  "samples": [
    {"aggregate_footprint_bytes": {"status": "available", "value": 42}}
  ]
}"""

WRONG_SHAPE_REPORT = (
    '{"outcome": {"kind": "child_exited", "code": 1}, '
    '"signals": [], "checkpoint": {}, "samples": []}'
)

EMERGENCY_REPORT = """{
  "outcome": {
    "kind": "policy_intervention", "at_ms": 900, "final_footprint_bytes": {"status": "unknown"}
  },
  "signals": [
    {"at_ms": 880, "signal": 9, "target": "owned_process_group",
     "result": "delivered", "reason": "footprint"}
  ],
  "checkpoint": {"status": "not_negotiated"},
  "transitions": [
    {"at_ms": 880, "from": "normal", "to": "emergency", "aggregate_footprint_bytes": 200}
  ],
  "samples": [{"aggregate_footprint_bytes": {"status": "available", "value": 200}}]
}"""


REPORT_WITH_QUALITY = """{
  "outcome": {"kind": "child_exited", "code": 0},
  "signals": [],
  "checkpoint": {"status": "not_negotiated"},
  "transitions": [],
  "configuration": {"sample_interval_ms": 50},
  "samples": [
    {"captured_at_ms": 10, "window_ms": 1,
     "aggregate_footprint_bytes": {"status": "available", "value": 10}},
    {"captured_at_ms": 20, "window_ms": 5,
     "aggregate_footprint_bytes": {"status": "available", "value": 20}},
    {"captured_at_ms": 30, "window_ms": 500,
     "aggregate_footprint_bytes": {"status": "partial", "known_subtotal": 5}}
  ]
}"""

REPORT_WITH_INTERVENTION = """{
  "outcome": {
    "kind": "policy_intervention", "at_ms": 900, "final_footprint_bytes": {"status": "unknown"}
  },
  "signals": [
    {"at_ms": 880, "signal": 9, "target": "owned_process_group",
     "result": "delivered", "reason": "footprint"}
  ],
  "checkpoint": {"status": "not_negotiated"},
  "transitions": [
    {"at_ms": 880, "from": "normal", "to": "emergency", "aggregate_footprint_bytes": 200}
  ],
  "configuration": {"max_footprint_bytes": 100, "emergency_footprint_bytes": 150},
  "samples": [
    {"captured_at_ms": 800, "window_ms": 2,
     "aggregate_footprint_bytes": {"status": "available", "value": 90}},
    {"captured_at_ms": 850, "window_ms": 3,
     "aggregate_footprint_bytes": {"status": "available", "value": 95}}
  ]
}"""


def multi_sample_report(values: list[int]) -> dict[str, object]:
    return {
        "outcome": {"kind": "child_exited", "code": 0},
        "signals": [],
        "checkpoint": {"status": "not_negotiated"},
        "transitions": [],
        "samples": [
            {"aggregate_footprint_bytes": {"status": "available", "value": v}} for v in values
        ],
    }


def make_done_arm(trial_root: Path, arm_id: str, report: dict[str, object]) -> None:
    """Fabricate a done arm directly on disk, bypassing run_arm, for derivation-source setup."""
    arm_root = trial_root / "arms" / arm_id
    attempt = arm_root / "attempt-1"
    attempt.mkdir(parents=True)
    (attempt / "report.json").write_text(json.dumps(report))
    (attempt / "result.json").write_text(json.dumps({"report_path": "report.json"}))
    orchestrator.mark_done(arm_root, attempt)


def write_fake_binary(path: Path) -> Path:
    path.write_text(FAKE_BINARY)
    path.chmod(0o755)
    return path


def write_fake_python_launcher(path: Path, report_filename: str) -> Path:
    path.write_text(
        f'#!/bin/sh\nprintf \'%s\' "$FAKE_REPORT_BODY" > "reports/{report_filename}"\nexit 0\n'
    )
    path.chmod(0o755)
    return path


def make_spec(arm_id: str = "l0") -> orchestrator.ArmSpec:
    return orchestrator.ArmSpec(
        arm_id=arm_id,
        mode="observe",
        argv=("/bin/echo", "hi"),
        sample_interval_ms=50,
        limit_bytes=None,
        wall_time_ms=None,
        validator=lambda report: v.assert_child_exit(report, 0),
        notes="test arm",
    )


def test_second_attempt_gets_a_fresh_directory(tmp_path: Path) -> None:
    """Bug: reusing attempt-1 collides with the retained journal — the arm can never be \
retried (review C3)."""
    arm_root = tmp_path / "arms" / "l3"
    first = orchestrator.next_attempt_dir(arm_root)
    assert first.name == "attempt-1"
    first.mkdir(parents=True)
    (first / "report.json.journal").write_text("stale journal from a killed attempt")
    second = orchestrator.next_attempt_dir(arm_root)
    assert second.name == "attempt-2"
    assert second != first


def test_status_written_only_after_artifacts(tmp_path: Path) -> None:
    """Bug: status written before copying — an arm marked done with no report."""
    arm_root = tmp_path / "arms" / "l0"
    attempt = arm_root / "attempt-1"
    attempt.mkdir(parents=True)
    with pytest.raises(FileNotFoundError):
        orchestrator.mark_done(arm_root, attempt)
    assert not (arm_root / "status").exists()
    (attempt / "result.json").write_text("{}")
    orchestrator.mark_done(arm_root, attempt)
    assert (arm_root / "status").exists()
    assert (arm_root / "status").read_text().strip() == "attempt-1"


def test_done_arm_is_skipped(tmp_path: Path) -> None:
    """Bug: is_done checks the report file instead of the status file — a failed-shape \
attempt is skipped forever."""
    arm_root = tmp_path / "arms" / "l0"
    attempt = arm_root / "attempt-1"
    attempt.mkdir(parents=True)
    (attempt / "report.json").write_text(
        WRONG_SHAPE_REPORT
    )  # a report exists, but validation never completed
    assert not orchestrator.is_done(arm_root)
    (attempt / "result.json").write_text("{}")
    orchestrator.mark_done(arm_root, attempt)
    assert orchestrator.is_done(arm_root)


def test_dry_run_prints_commands_and_launches_nothing(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """Bug: dry-run spawns the subprocess instead of only printing its argv."""
    marker = tmp_path / "launched"
    binary = write_fake_binary(tmp_path / "fake-mlx-guard.sh")
    spec = make_spec("l0")
    result = orchestrator.run_arm(spec, tmp_path, binary, dry_run=True)
    captured = capsys.readouterr()
    assert "l0" in captured.out
    assert "mlx-guard" in captured.out or binary.name in captured.out
    assert not marker.exists()
    assert not (tmp_path / "arms").exists()
    assert result.dry_run is True
    assert result.exit_code is None


def test_run_arm_marks_done_on_a_valid_report(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """End-to-end sanity check for the real subprocess path the other tests assume works."""
    binary = write_fake_binary(tmp_path / "fake-mlx-guard.sh")
    monkeypatch.setenv("FAKE_REPORT_BODY", UNEVENTFUL_REPORT)
    monkeypatch.setenv("FAKE_EXIT_CODE", "0")
    spec = make_spec("l0")
    result = orchestrator.run_arm(spec, tmp_path, binary, dry_run=False)
    assert result.exit_code == 0
    assert result.validator_ok is True
    arm_root = tmp_path / "arms" / "l0"
    assert orchestrator.is_done(arm_root)
    assert (arm_root / "attempt-1" / "result.json").exists()


def test_assemble_collects_python_mode_reports(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Bug: hardcoded `report.json` skips C1/C2's reports (they live at reports/<arm>.json)."""
    launcher = write_fake_python_launcher(tmp_path / "fake_launcher.sh", "c1.json")
    monkeypatch.setenv("FAKE_REPORT_BODY", UNEVENTFUL_REPORT)
    spec = orchestrator.ArmSpec(
        arm_id="C1",
        mode="python",
        argv=(str(launcher),),
        sample_interval_ms=50,
        limit_bytes=None,
        wall_time_ms=None,
        validator=v.assert_uneventful,
        notes="test python-mode arm",
    )
    result = orchestrator.run_arm(spec, tmp_path, Path("/unused-binary"), dry_run=False)
    assert result.validator_ok is True
    assert orchestrator.is_done(tmp_path / "arms" / "C1")

    bundle = tmp_path / "bundle"
    written = orchestrator.assemble(tmp_path, bundle)
    dest = bundle / "reports" / "C1.json"
    assert dest.exists()
    assert dest in written


def test_derive_headroom_uses_the_max_over_named_arms(tmp_path: Path) -> None:
    """Bug: uses the first named arm only."""
    make_done_arm(tmp_path, "L1a", multi_sample_report([100]))
    make_done_arm(tmp_path, "L1b", multi_sample_report([200]))
    derived = orchestrator.compute_derivation(tmp_path, ["L1a", "L1b"], "headroom")
    assert derived["limit_bytes"] == bands.headroom_limit(200, 125)
    assert derived["source_peak_bytes"] == 200


def test_derive_graceful_writes_prediction_before_launch(tmp_path: Path) -> None:
    """Bug: prediction computed from the run's own samples after the fact — circular."""
    make_done_arm(tmp_path, "L1a", multi_sample_report([90, 95, 100, 98, 97]))
    binary = tmp_path / "fake-mlx-guard.sh"
    binary.write_text(PREDICTION_CHECKING_BINARY)
    binary.chmod(0o755)
    spec = orchestrator.ArmSpec(
        arm_id="L3",
        mode="run",
        argv=("/bin/echo", "hi"),
        sample_interval_ms=50,
        limit_bytes=None,
        wall_time_ms=None,
        validator=lambda report: None,
        notes="test",
    )
    orchestrator.run_derived_arm(
        spec, tmp_path, binary, source_arm_ids=["L1a"], derivation="graceful", dry_run=False
    )
    assert (tmp_path / "arms" / "L3" / "prediction.json").exists()
    report = json.loads((tmp_path / "arms" / "L3" / "attempt-1" / "report.json").read_text())
    assert report["outcome"]["code"] == 0  # prediction.json already existed when the binary ran


def test_derive_refuses_an_undone_source_arm(tmp_path: Path) -> None:
    """Bug: reads a source arm's report before checking whether it finished."""
    with pytest.raises(orchestrator.DerivationError):
        orchestrator.compute_derivation(tmp_path, ["L1a"], "headroom")


def test_band_mismatch_is_recorded_not_failed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Bug: a mismatch between predicted and observed band blocks mark_done as a hard failure
    instead of being recorded as `band_mismatch` while the run still counts as evidence."""
    make_done_arm(tmp_path, "L1a", multi_sample_report([90, 95, 100, 98, 97]))
    binary = write_fake_binary(tmp_path / "fake-mlx-guard.sh")
    monkeypatch.setenv("FAKE_REPORT_BODY", EMERGENCY_REPORT)
    monkeypatch.setenv("FAKE_EXIT_CODE", "0")
    spec = orchestrator.ArmSpec(
        arm_id="L3",
        mode="run",
        argv=("/bin/echo", "hi"),
        sample_interval_ms=50,
        limit_bytes=None,
        wall_time_ms=None,
        validator=v.assert_uneventful,  # irrelevant placeholder; graceful mode overrides it
        notes="test",
    )
    result = orchestrator.run_derived_arm(
        spec, tmp_path, binary, source_arm_ids=["L1a"], derivation="graceful", dry_run=False
    )
    assert result.predicted_band == "graceful"
    assert result.observed_band == "emergency"
    assert result.band_mismatch is True
    assert result.validator_ok is True
    assert orchestrator.is_done(tmp_path / "arms" / "L3")


def test_only_can_carry_c2s_resume_paths_through_to_the_launcher(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """Bug: `orchestrator.py` has no `--resume-report`/`--c1-checkpoints` flags, so a real C2
    resume launch can never be driven through the CLI — only a hand-edited `ArmSpec`."""
    binary = write_fake_binary(tmp_path / "fake-mlx-guard.sh")
    argv = [
        "--trial-root",
        str(tmp_path),
        "--binary",
        str(binary),
        "--only",
        "C2",
        "--limit-bytes",
        "1",
        "--checkpoint-timeout-ms",
        "1000",
        "--resume-report",
        "../../C1/attempt-1/reports/c1.json",
        "--c1-checkpoints",
        "../../C1/attempt-1/checkpoints",
        "--dry-run",
    ]
    exit_code = orchestrator.main(argv)
    captured = capsys.readouterr()
    assert exit_code == 0
    assert "--resume-report" in captured.out
    assert "../../C1/attempt-1/reports/c1.json" in captured.out
    assert "--c1-checkpoints" in captured.out
    assert "../../C1/attempt-1/checkpoints" in captured.out

    rejected = orchestrator.main(
        [
            "--trial-root",
            str(tmp_path),
            "--binary",
            str(binary),
            "--resume-report",
            "../../C1/attempt-1/reports/c1.json",
            "--c1-checkpoints",
            "../../C1/attempt-1/checkpoints",
            "--dry-run",
        ]
    )
    assert rejected == 2


def test_result_json_carries_the_three_measurements(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Bug: metrics computed but not persisted, so the bundle cannot cite them."""
    binary = write_fake_binary(tmp_path / "fake-mlx-guard.sh")
    monkeypatch.setenv("FAKE_REPORT_BODY", REPORT_WITH_QUALITY)
    monkeypatch.setenv("FAKE_EXIT_CODE", "0")
    orchestrator.run_arm(make_spec("l0"), tmp_path, binary, dry_run=False)
    result_l0 = json.loads((tmp_path / "arms" / "l0" / "attempt-1" / "result.json").read_text())
    assert result_l0["first_sample_captured_at_ms"] == 10
    assert result_l0["unavailable_samples"] == 1
    assert result_l0["sample_window_p95_ms"] == 500
    assert result_l0["sample_window_max_ms"] == 500
    assert result_l0["intervention_overshoot"] is None

    monkeypatch.setenv("FAKE_REPORT_BODY", REPORT_WITH_INTERVENTION)
    spec_l1 = orchestrator.ArmSpec(
        arm_id="l1",
        mode="run",
        argv=("/bin/echo", "hi"),
        sample_interval_ms=50,
        limit_bytes=100,
        wall_time_ms=None,
        validator=v.assert_emergency_footprint,
        notes="test arm with an intervention",
    )
    orchestrator.run_arm(spec_l1, tmp_path, binary, dry_run=False)
    result_l1 = json.loads((tmp_path / "arms" / "l1" / "attempt-1" / "result.json").read_text())
    assert result_l1["intervention_overshoot"] == {
        "observed_at_intervention_bytes": 95,
        "limit_bytes": 100,
        "overshoot_bytes": -5,
        "emergency_threshold_bytes": 150,
        "within_band": True,
    }

    bundle = tmp_path / "bundle"
    orchestrator.assemble(tmp_path, bundle)
    arms_index = {row["arm_id"]: row for row in json.loads((bundle / "arms.json").read_text())}
    assert arms_index["l0"]["sample_window_max_ms"] == 500
    assert arms_index["l1"]["intervention_overshoot"]["overshoot_bytes"] == -5


def test_version_pin_refuses_a_changed_triple(tmp_path: Path) -> None:
    """Bug: L2 launched against a different mlx-lm than L1 calibrated."""
    orchestrator.record_version(tmp_path, "L", "mlx-lm==0.31.3")
    orchestrator.check_version(tmp_path, "L", "mlx-lm==0.31.3")  # same triple: no complaint
    with pytest.raises(orchestrator.VersionPinError):
        orchestrator.check_version(tmp_path, "L", "mlx-lm==0.32.0")
