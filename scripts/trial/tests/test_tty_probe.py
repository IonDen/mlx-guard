"""Tty-probe tests. Each docstring names the one-line bug that turns the test red.

T0 must run against a real pseudo-terminal and a real mlx-guard binary — a skipped RED is not a
RED, so the binary path is read from ``MLX_GUARD_TRIAL_BINARY`` and this test FAILS, never skips,
when that variable is unset.
"""

import os
from pathlib import Path

import pytest
import tty_probe
import validators as v


def test_bare_pty_is_refused_and_the_redirect_remedy_runs(tmp_path: Path) -> None:
    """T0: a terminal on stdin is refused with 64 and the documented message; `< /dev/null` runs."""
    binary = os.environ.get("MLX_GUARD_TRIAL_BINARY")
    if not binary:
        pytest.fail(
            "MLX_GUARD_TRIAL_BINARY is unset — this test must fail, not skip, without a real binary"
        )
    probe = tty_probe.run_probe(Path(binary), tmp_path, ("/usr/bin/true",))
    v.assert_tty_probe(probe)


def test_missing_binary_is_a_harness_error(tmp_path: Path) -> None:
    """Bug: a failing execv returns into the harness's Python stack — the forked child re-enters the
    read loop and waitpids a process it never forked."""
    with pytest.raises(RuntimeError):
        tty_probe.run_probe(Path("/nonexistent"), tmp_path, ("/usr/bin/true",))


def test_pty_read_loop_has_a_wall_backstop() -> None:
    """Bug: no deadline — a child holding the slave open hangs the 10 s arm forever."""
    with pytest.raises(TimeoutError):
        tty_probe._spawn_under_pty(["/bin/sleep", "5"], stdin_devnull=False, timeout_s=0.5)
