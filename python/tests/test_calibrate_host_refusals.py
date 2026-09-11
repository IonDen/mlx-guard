"""Tests for the argument checks that precede any build in the one-command host calibration."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts/calibrate-host.sh"


def calibrate(
    output: str, *, endurance_seconds: str | None = None, check_only: bool = False
) -> subprocess.CompletedProcess[str]:
    """Invoke the script with one output argument and return the completed process."""
    env = dict(os.environ)
    env.pop("MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS", None)
    env.pop("MLX_GUARD_CALIBRATION_CHECK_ARGUMENTS_ONLY", None)
    if endurance_seconds is not None:
        env["MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS"] = endurance_seconds
    if check_only:
        env["MLX_GUARD_CALIBRATION_CHECK_ARGUMENTS_ONLY"] = "1"
    return subprocess.run(
        [str(SCRIPT), output],
        capture_output=True,
        text=True,
        check=False,
        env=env,
        timeout=30,
    )


class CalibrateHostRefusalTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red.

    Every refusal exercised here is checked before the script builds anything or probes the
    hardware, so the tests cost milliseconds; a refusal that came after a cargo build would time
    out instead of failing fast. The positive controls use the argument-check-only mode, which
    exits right after the same checks, so a widened check goes red without a build either.
    """

    def test_output_inside_repository_is_refused(self) -> None:
        # Red if the outside-the-repository check is dropped: the staging directory would dirty
        # the worktree and block the resume the script exists to provide.
        target = REPO_ROOT / "evidence" / "never-created"
        completed = calibrate(str(target))
        self.assertEqual(completed.returncode, 64, completed.stderr)
        self.assertIn("outside the repository", completed.stderr)
        self.assertFalse(target.exists())
        self.assertFalse(target.with_name("never-created.staging").exists())

    def test_sibling_directory_sharing_the_repository_prefix_is_accepted(self) -> None:
        # Red if the inside-repository check becomes a prefix or substring match: a sibling named
        # `<repo>-evidence` is outside the checkout and must be accepted.
        sibling = REPO_ROOT.with_name(REPO_ROOT.name + "-evidence-never-created")
        completed = calibrate(str(sibling), endurance_seconds="1800", check_only=True)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("arguments accepted", completed.stdout)
        self.assertFalse(sibling.exists())
        self.assertFalse(sibling.with_name(sibling.name + ".staging").exists())

    def test_existing_output_directory_is_refused(self) -> None:
        # Red if an existing output directory is silently reused or overwritten.
        with tempfile.TemporaryDirectory() as existing:
            completed = calibrate(existing)
        self.assertEqual(completed.returncode, 64, completed.stderr)
        self.assertIn("already exists", completed.stderr)

    def test_existing_logs_sibling_is_refused(self) -> None:
        # Red if a leftover `<out>.logs` from an earlier run is nested into instead of refused:
        # the transcripts of two runs would then be mixed under one directory.
        with tempfile.TemporaryDirectory() as parent:
            output = Path(parent) / "bundle"
            (Path(parent) / "bundle.logs").mkdir()
            completed = calibrate(str(output), check_only=True)
        self.assertEqual(completed.returncode, 64, completed.stderr)
        self.assertIn(".logs", completed.stderr)

    def test_non_integer_endurance_override_is_refused(self) -> None:
        # Red if the endurance override is passed through unvalidated: the endurance test would
        # fail after the four chunks before it had already run.
        with tempfile.TemporaryDirectory() as parent:
            completed = calibrate(str(Path(parent) / "bundle"), endurance_seconds="abc")
        self.assertEqual(completed.returncode, 64, completed.stderr)
        self.assertIn("MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS", completed.stderr)

    def test_endurance_override_above_thirty_minutes_is_refused(self) -> None:
        # Red if the upper bound is dropped: the published bound is a 30-minute run, and a longer
        # override would not be the measurement the evidence README describes.
        with tempfile.TemporaryDirectory() as parent:
            completed = calibrate(str(Path(parent) / "bundle"), endurance_seconds="1801")
        self.assertEqual(completed.returncode, 64, completed.stderr)
        self.assertIn("MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS", completed.stderr)

    def test_endurance_at_exactly_thirty_minutes_is_accepted(self) -> None:
        # Red if the bound becomes exclusive: 1800 s is the published duration and must pass.
        with tempfile.TemporaryDirectory() as parent:
            completed = calibrate(
                str(Path(parent) / "bundle"), endurance_seconds="1800", check_only=True
            )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("1800", completed.stdout)


if __name__ == "__main__":
    unittest.main()
