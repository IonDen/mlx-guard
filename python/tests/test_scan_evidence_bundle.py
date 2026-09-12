"""Tests for the identifier and local-path scan that gates publishing an evidence bundle."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
SCRIPT = Path(__file__).resolve().parents[2] / "scripts/scan-evidence-bundle.sh"


def scan(
    directory: Path, *literals: str, home: str | None = None
) -> subprocess.CompletedProcess[str]:
    """Scan one directory, with optional extra literals, and return the completed process."""
    env = dict(os.environ)
    if home is not None:
        env["HOME"] = home
    return subprocess.run(
        [str(SCRIPT), str(directory), *literals],
        capture_output=True,
        text=True,
        check=False,
        env=env,
        timeout=30,
    )


class ScanEvidenceBundleTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red."""

    def setUp(self) -> None:
        self._temporary = tempfile.TemporaryDirectory()
        self.bundle = Path(self._temporary.name) / "bundle"
        self.bundle.mkdir()
        (self.bundle / "footprint.json").write_text('{"schema_version": 1}\n')
        (self.bundle / "scenarios" / "reports").mkdir(parents=True)
        (self.bundle / "scenarios" / "reports" / ".x.json.journal").write_bytes(b"MLXJ\x00\x01")

    def tearDown(self) -> None:
        self._temporary.cleanup()

    def test_clean_bundle_passes(self) -> None:
        # Red if a clean bundle is refused, for example because "no match" is read as an error.
        completed = scan(self.bundle)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stdout, "")

    def test_empty_directory_passes(self) -> None:
        # Red if the scan of a directory with no files is read as a match (the xargs-with-no-input
        # inversion) instead of a clean pass.
        empty = Path(self._temporary.name) / "empty"
        empty.mkdir()
        completed = scan(empty)
        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_identifier_label_in_a_nested_journal_is_refused(self) -> None:
        # Red if the scan skips dotfiles or nested directories: the journals under
        # scenarios/reports/ are both.
        journal = self.bundle / "scenarios" / "reports" / ".x.json.journal"
        journal.write_bytes(b"MLXJ\x00Hardware UUID: 1234\x00")
        completed = scan(self.bundle)
        self.assertEqual(completed.returncode, 70, completed.stderr)
        self.assertIn(".x.json.journal", completed.stdout)

    def test_uuid_shaped_value_is_refused(self) -> None:
        # Red if the UUID-shaped pattern is dropped: a serial or platform UUID value with no label
        # in front of it must still be caught.
        (self.bundle / "footprint.json").write_text(
            '{"host": "0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b"}\n'
        )
        completed = scan(self.bundle)
        self.assertEqual(completed.returncode, 70, completed.stderr)

    def test_extra_literal_is_matched_as_a_fixed_string(self) -> None:
        # Red if the extra literals (hostname, user name, serial value, checkout path) are not
        # searched, or are compiled as regular expressions so that "denis.mac" matches "denisXmac"
        # only by accident and a literal with brackets breaks the scan.
        (self.bundle / "runtime.json").write_text('{"note": "captured on host denis-mbp[2]"}\n')
        completed = scan(self.bundle, "unrelated", "denis-mbp[2]")
        self.assertEqual(completed.returncode, 70, completed.stderr)
        self.assertIn("runtime.json", completed.stdout)

    def test_home_with_regex_metacharacter_still_catches_a_local_path(self) -> None:
        # Red if HOME is spliced into a regular expression: an unbalanced bracket in it makes
        # grep fail with a syntax error, which an unchecked exit status reads as "clean", and the
        # whole scan is silently defeated.
        (self.bundle / "runtime.json").write_text('{"path": "/Users/someone/secret"}\n')
        completed = scan(self.bundle, home="/srv/build[oops")
        self.assertEqual(completed.returncode, 70, completed.stderr)
        self.assertIn("runtime.json", completed.stdout)

    def test_home_path_itself_is_refused(self) -> None:
        # Red if the invoking user's home directory is not on the literal list: a checkout under a
        # non-/Users home would otherwise publish its path.
        (self.bundle / "runtime.json").write_text('{"path": "/srv/homes/ion/mlx-guard/x"}\n')
        completed = scan(self.bundle, home="/srv/homes/ion")
        self.assertEqual(completed.returncode, 70, completed.stderr)

    def test_empty_literal_is_skipped(self) -> None:
        # Red if an empty literal (a hardware field the host did not report) is handed to grep,
        # where an empty fixed string matches every file and a clean bundle is refused.
        completed = scan(self.bundle, "")
        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_excluded_directory_is_not_scanned(self) -> None:
        # Red if --exclude-dir is ignored: the calibrator scans its staging directory with the
        # transcripts still inside, and those name local paths by nature.
        logs = self.bundle / "logs"
        logs.mkdir()
        (logs / "footprint.log").write_text("Running /Users/someone/mlx-guard/target/debug/x\n")
        refused = scan(self.bundle)
        self.assertEqual(refused.returncode, 70, refused.stderr)
        completed = subprocess.run(
            [str(SCRIPT), "--exclude-dir", "logs", str(self.bundle)],
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_refusal_names_the_literal_that_matched(self) -> None:
        # Red if the refusal lists only files: an operator whose host name is an ordinary word
        # could not tell a false positive from a leak without knowing which literal fired.
        (self.bundle / "runtime.json").write_text('{"note": "host studio-mbp"}\n')
        completed = scan(self.bundle, "studio-mbp")
        self.assertEqual(completed.returncode, 70, completed.stderr)
        self.assertIn("matched literal: studio-mbp", completed.stderr)

    def test_unset_home_still_scans(self) -> None:
        # Red if an unset HOME aborts the script with a bash unbound-variable error (exit 1)
        # instead of scanning without that literal.
        env = {k: v for k, v in os.environ.items() if k != "HOME"}
        completed = subprocess.run(
            [str(SCRIPT), str(self.bundle)],
            capture_output=True,
            text=True,
            check=False,
            env=env,
            timeout=30,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_missing_directory_is_an_error_not_a_pass(self) -> None:
        # Red if a nonexistent directory (a typo in the caller) is reported clean.
        completed = scan(Path(self._temporary.name) / "nowhere")
        self.assertEqual(completed.returncode, 70, completed.stdout)


if __name__ == "__main__":
    unittest.main()
