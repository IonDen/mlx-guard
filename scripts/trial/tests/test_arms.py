"""Arm-table tests. Each docstring names the one-line bug that turns the test red."""

import sys

import arms  # scripts/trial on sys.path via conftest.py


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
