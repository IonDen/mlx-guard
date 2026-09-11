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
    output: str, *, endurance_seconds: str | None = None
) -> subprocess.CompletedProcess[str]:
    """Invoke the script with one output argument and return the completed process."""
    env = dict(os.environ)
    env.pop("MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS", None)
    if endurance_seconds is not None:
        env["MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS"] = endurance_seconds
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
    out instead of failing fast.
    """

    def test_output_inside_repository_is_refused(self) -> None:
        # Red if the outside-the-repository check is dropped: the staging directory would dirty
        # the worktree and block the resume the script exists to provide.
        completed = calibrate(str(REPO_ROOT / "evidence" / "never-created"))
        self.assertEqual(completed.returncode, 64, completed.stderr)
        self.assertIn("outside the repository", completed.stderr)
        self.assertFalse((REPO_ROOT / "evidence" / "never-created").exists())

    def test_existing_output_directory_is_refused(self) -> None:
        # Red if an existing output directory is silently reused or overwritten.
        with tempfile.TemporaryDirectory() as existing:
            completed = calibrate(existing)
        self.assertEqual(completed.returncode, 64, completed.stderr)
        self.assertIn("already exists", completed.stderr)

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


if __name__ == "__main__":
    unittest.main()
