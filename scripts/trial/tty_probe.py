"""T0: exercise the interactive-terminal refusal through a real pseudo-terminal."""

import os
import pty
import select
import signal
import time
from pathlib import Path

EXEC_FAILED = (
    127  # the child's only possible exit when exec itself fails; a harness error, never a result
)


def _spawn_under_pty(
    argv: list[str], *, stdin_devnull: bool, timeout_s: float = 30.0
) -> tuple[int, str]:
    pid, master = pty.fork()
    if pid == 0:  # child: the pty is stdin/stdout/stderr; optionally redirect stdin only
        try:
            if stdin_devnull:
                devnull = os.open("/dev/null", os.O_RDONLY)
                os.dup2(devnull, 0)
            os.execv(argv[0], argv)  # noqa: S606 — argv[0] is an absolute path the caller built, never a shell string
        except OSError:
            os._exit(EXEC_FAILED)  # a failed exec must never return into the harness's own stack
    chunks: list[bytes] = []
    deadline = time.monotonic() + timeout_s
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            raise TimeoutError(f"pty child exceeded {timeout_s}s")
        ready, _, _ = select.select([master], [], [], remaining)
        if not ready:
            continue
        try:
            data = os.read(master, 4096)
        except OSError:
            break
        if not data:
            break
        chunks.append(data)
    _, status = os.waitpid(pid, 0)
    code = os.waitstatus_to_exitcode(status)
    if code == EXEC_FAILED and not chunks:
        raise RuntimeError(
            f"could not exec {argv[0]!r} under the pty (harness error, not a result)"
        )
    return code, b"".join(chunks).decode(errors="replace").strip()


def run_probe(binary: Path, workdir: Path, command: tuple[str, ...]) -> dict[str, object]:
    """Run a bare pty launch (expect 64) and a `< /dev/null` remedy (expect 0), under ``workdir``.

    Returns the two exit codes plus each launch's captured pty output.
    """
    reports = workdir / "reports"
    reports.mkdir(mode=0o700, exist_ok=True)
    bare = [str(binary), "observe", "--report", str(reports / "t0-refused.json"), "--", *command]
    remedy = [
        str(binary),
        "observe",
        "--report",
        str(reports / "t0-redirected.json"),
        "--",
        *command,
    ]
    bare_exit, bare_out = _spawn_under_pty(bare, stdin_devnull=False)
    remedy_exit, remedy_out = _spawn_under_pty(remedy, stdin_devnull=True)
    return {
        "bare_exit": bare_exit,
        "bare_stderr": bare_out,
        "remedy_exit": remedy_exit,
        "remedy_stderr": remedy_out,
    }
