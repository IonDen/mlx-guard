import json
import os
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import mlx_guard


class InstalledPackageTests(unittest.TestCase):
    def test_package_and_binary_versions_match(self) -> None:
        binary = mlx_guard.binary_path()

        self.assertTrue(binary.is_file())
        self.assertFalse(binary.is_symlink())
        self.assertTrue(os.access(binary, os.X_OK))
        self.assertEqual(mlx_guard.binary_version(), mlx_guard.__version__)
        completed = subprocess.run(
            [binary, "--version"],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.stderr, "")
        self.assertEqual(completed.stdout, f"mlx-guard {mlx_guard.__version__}\n")

    def test_discovery_ignores_path_and_current_directory_shadow(self) -> None:
        expected = mlx_guard.binary_path()
        with tempfile.TemporaryDirectory() as temporary:
            shadow = Path(temporary, "mlx-guard")
            shadow.write_text("#!/bin/sh\necho shadow\n", encoding="utf-8")
            shadow.chmod(shadow.stat().st_mode | stat.S_IXUSR)
            with mock.patch.dict(os.environ, {"PATH": temporary}):
                previous = Path.cwd()
                try:
                    os.chdir(temporary)
                    self.assertEqual(mlx_guard.binary_path(), expected)
                    self.assertEqual(mlx_guard.binary_version(), mlx_guard.__version__)
                finally:
                    os.chdir(previous)

    def test_missing_packaged_binary_is_rejected(self) -> None:
        binary = mlx_guard.binary_path()
        saved = binary.with_name(f"{binary.name}.saved-for-test")
        binary.rename(saved)
        try:
            with self.assertRaisesRegex(
                mlx_guard.BinaryDiscoveryError,
                "^packaged supervisor is missing$",
            ):
                mlx_guard.binary_path()
        finally:
            saved.rename(binary)

    def test_non_executable_packaged_binary_is_rejected(self) -> None:
        binary = mlx_guard.binary_path()
        original_mode = binary.stat().st_mode
        binary.chmod(original_mode & ~0o111)
        try:
            with self.assertRaisesRegex(
                mlx_guard.BinaryDiscoveryError,
                "^packaged supervisor is not executable$",
            ):
                mlx_guard.binary_path()
        finally:
            binary.chmod(original_mode)

    def test_symlinked_packaged_binary_is_rejected(self) -> None:
        binary = mlx_guard.binary_path()
        saved = binary.with_name(f"{binary.name}.saved-for-test")
        binary.rename(saved)
        binary.symlink_to(saved.name)
        try:
            with self.assertRaisesRegex(
                mlx_guard.BinaryDiscoveryError,
                "^packaged supervisor is not a regular file$",
            ):
                mlx_guard.binary_path()
        finally:
            binary.unlink()
            saved.rename(binary)

    def test_python_client_crash_does_not_orphan_supervision(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary, "report.json")
            client = subprocess.Popen(
                [sys.executable, __file__, "--crash-probe", str(report)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            assert client.stdout is not None
            supervisor_pid = int(client.stdout.readline().strip())
            os.kill(client.pid, signal.SIGKILL)
            _, client_stderr = client.communicate(timeout=5)
            self.assertEqual(client.returncode, -signal.SIGKILL)
            # The supervisor prints the emergency-band banner on the inherited stderr as it
            # launches the workload. Whether that reaches the pipe before the client is killed is a
            # race, so tolerate it being present or absent; nothing else should appear on stderr.
            for line in client_stderr.splitlines():
                if line:
                    self.assertIn("emergency KILL", line, client_stderr)

            deadline = time.monotonic() + 5
            while not report.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(report.exists(), "supervisor did not finalize after client crash")
            payload = json.loads(report.read_text(encoding="utf-8"))
            # The default on-parent-exit policy is "terminate": the supervisor notices
            # its launching client died, terminates the owned worker group, and reports
            # the intervention rather than the worker's own (would-be) natural exit.
            self.assertEqual(payload["outcome"]["kind"], "policy_intervention")
            reasons = {record.get("reason") for record in payload["signals"]}
            self.assertIn("parent_exit", reasons)

            while _process_exists(supervisor_pid) and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertFalse(_process_exists(supervisor_pid))

    def test_python_client_crash_with_detach_leaves_supervision_running(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary, "report.json")
            client = subprocess.Popen(
                [sys.executable, __file__, "--crash-probe", str(report), "detach"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
            )
            assert client.stdout is not None
            supervisor_pid = int(client.stdout.readline().strip())
            os.kill(client.pid, signal.SIGKILL)
            # A detached supervisor keeps the launcher's inherited stdout pipe open
            # for as long as it (and its worker) stay alive, so communicate() would
            # block on the very survival this test is proving; wait() only reaps the
            # launcher's own exit code.
            client.wait(timeout=5)
            client.stdout.close()
            self.assertEqual(client.returncode, -signal.SIGKILL)

            try:
                # A window well inside the worker's own sleep: if "detach" acted like
                # "terminate" the supervisor would already be gone by now.
                time.sleep(1.0)
                self.assertTrue(
                    _process_exists(supervisor_pid),
                    "detached supervision must survive the client's death",
                )
                self.assertFalse(report.exists(), "detached supervision must not finalize early")
            finally:
                # Detach leaves the tree running with no owning parent left; the test
                # tears it down itself so no supervised process survives the test.
                os.kill(supervisor_pid, signal.SIGTERM)
                deadline = time.monotonic() + 5
                while _process_exists(supervisor_pid) and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertFalse(_process_exists(supervisor_pid))

    def test_tampered_packaged_binary_is_rejected(self) -> None:
        binary = mlx_guard.binary_path()
        original = binary.read_bytes()
        original_mode = binary.stat().st_mode
        try:
            binary.write_bytes(b"#!/bin/sh\necho mlx-guard 0.1.0\n")
            binary.chmod(original_mode)
            with self.assertRaisesRegex(
                mlx_guard.PackageIntegrityError,
                "^packaged supervisor does not match file integrity metadata$",
            ):
                mlx_guard.binary_path()
        finally:
            binary.write_bytes(original)
            binary.chmod(original_mode)


def _process_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def _run_crash_probe(report: str, on_parent_exit: str | None) -> None:
    # Both paths use a long-sleep worker (well past the test's own patience window):
    # a detached run must outlive it, and the terminate run ends at TERM/KILL
    # detection rather than the worker's natural exit, so a long sleep costs no wall
    # time there either — it just closes a race where a starved CI runner could let
    # the worker exit naturally before detection, flipping the outcome to
    # child_exited instead of policy_intervention.
    duration = "5"
    supervisor = mlx_guard.start(
        mlx_guard.RunConfig(
            command=("/bin/sleep", duration),
            report=Path(report),
            max_footprint_bytes=1024**4,
            sample_interval_ms=10,
            on_parent_exit=on_parent_exit,
        )
    )
    print(supervisor.pid, flush=True)
    time.sleep(30)


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "--crash-probe":
        _run_crash_probe(sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
    else:
        unittest.main()
