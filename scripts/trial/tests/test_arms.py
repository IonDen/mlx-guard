"""Arm-table tests. Each docstring names the one-line bug that turns the test red."""

import sys

import arms  # scripts/trial on sys.path via conftest.py
import pytest


def _value_after(argv: tuple[str, ...], flag: str) -> str:
    """Return the token immediately following ``flag`` in ``argv``."""
    index = argv.index(flag)
    return argv[index + 1]


def test_c_arms_run_under_the_orchestrators_interpreter() -> None:
    """Bug: argv[0] is ``"python3"`` (or the script's own shebang) — the system interpreter has
    neither ``mlx`` nor ``mlx_guard``, so every cooperative arm dies at import while the dry-run
    output looks fine. The orchestrator runs under the trial venv, so its interpreter is the one."""
    specs = {
        s.arm_id: s
        for s in arms.build_arms(
            limit_bytes={"C1": 1, "C2": 1},
            wall_time_ms={"C1": 1},
            checkpoint_timeout_ms={"C1": 1000, "C2": 1000},
        )
    }
    for arm_id in ("C0", "C1", "C2"):
        argv = specs[arm_id].argv
        assert argv[0] == sys.executable, (arm_id, argv[:2])
        assert argv[1] == arms.RESUMABLE_LORA, (arm_id, argv[:2])


def test_c_arm_launch_flags_render_resolved_values_and_placeholders() -> None:
    """Bug: `_required_flag` call sites dropped → `launch` argparse rejects every real C1/C2
    launch (`--max-footprint-bytes`/`--checkpoint-timeout-ms` are required), while the dry-run
    output looks complete."""
    specs = {
        s.arm_id: s
        for s in arms.build_arms(
            limit_bytes={"C1": 123, "C2": 456},
            wall_time_ms={"C1": 789},
            checkpoint_timeout_ms={"C1": 1000, "C2": 2000},
        )
    }
    c1 = specs["C1"].argv
    c2 = specs["C2"].argv
    assert _value_after(c1, "--max-footprint-bytes") == "123"
    assert _value_after(c1, "--wall-time-ms") == "789"
    assert _value_after(c1, "--checkpoint-timeout-ms") == "1000"
    assert _value_after(c2, "--max-footprint-bytes") == "456"
    assert _value_after(c2, "--checkpoint-timeout-ms") == "2000"
    assert "--wall-time-ms" not in c2

    unresolved = {s.arm_id: s for s in arms.build_arms()}
    c1_unresolved = unresolved["C1"].argv
    c2_unresolved = unresolved["C2"].argv
    assert _value_after(c1_unresolved, "--max-footprint-bytes") == "<limit>"
    assert _value_after(c1_unresolved, "--checkpoint-timeout-ms") == "<checkpoint-timeout>"
    assert _value_after(c2_unresolved, "--max-footprint-bytes") == "<limit>"
    assert _value_after(c2_unresolved, "--checkpoint-timeout-ms") == "<checkpoint-timeout>"


def test_c2_argv_carries_the_resume_paths_when_both_are_given() -> None:
    """Bug: the strings never reach the launcher, so "C2" trains from scratch and the marker
    never appears."""
    specs = {
        s.arm_id: s
        for s in arms.build_arms(
            limit_bytes={"C2": 1},
            checkpoint_timeout_ms={"C2": 1000},
            resume_report={"C2": "../../C1/attempt-1/reports/c1.json"},
            c1_checkpoints={"C2": "../../C1/attempt-1/checkpoints"},
        )
    }
    argv = specs["C2"].argv
    assert _value_after(argv, "--resume-report") == "../../C1/attempt-1/reports/c1.json"
    assert _value_after(argv, "--c1-checkpoints") == "../../C1/attempt-1/checkpoints"


def test_c2_refuses_half_a_resume_pair() -> None:
    """Bug: a missing `--c1-checkpoints` produces a launch that `raise SystemExit`s only after
    the model has loaded."""
    with pytest.raises(ValueError, match="both"):
        arms.build_arms(resume_report={"C2": "r.json"})
    with pytest.raises(ValueError, match="both"):
        arms.build_arms(c1_checkpoints={"C2": "ck"})
