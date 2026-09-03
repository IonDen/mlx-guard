"""Orchestrator tests. Each docstring names the one-line bug that turns the test red.

The fake binary is a real ``/bin/sh`` script (never mlx-guard itself) that writes a minimal
report to whatever path follows ``--report``, controlled by two environment variables so a
single script can stand in for both a passing and a failing arm.
"""

from pathlib import Path

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


def write_fake_binary(path: Path) -> Path:
    path.write_text(FAKE_BINARY)
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


def test_version_pin_refuses_a_changed_triple(tmp_path: Path) -> None:
    """Bug: L2 launched against a different mlx-lm than L1 calibrated."""
    orchestrator.record_version(tmp_path, "L", "mlx-lm==0.31.3")
    orchestrator.check_version(tmp_path, "L", "mlx-lm==0.31.3")  # same triple: no complaint
    with pytest.raises(orchestrator.VersionPinError):
        orchestrator.check_version(tmp_path, "L", "mlx-lm==0.32.0")
