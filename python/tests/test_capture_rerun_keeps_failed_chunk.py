"""A capture rerun keeps a failed chunk's artifact and transcript under a numbered attempt name.

The two capture scripts are copied unchanged into a scratch repository whose `cargo`, `git`,
hardware probes and helper scripts are small stand-ins, so the real `run_chunk` runs end to end in
well under a second per run: nothing is built and no measurement runs. The stand-in `cargo test`
behaves like the real chunk tests in the one way that matters here: it writes its artifact before
judging its bound, then fails with exit 101 when told to.
"""

import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_DIRECTORY = REPO_ROOT / "scripts"

SOAK = "soak-reference-host.sh"
CALIBRATE = "calibrate-host.sh"

# The chunk test names the scripts pass to `cargo test`.
ESCAPING_CHURN = "escaping_churn_supervisor_footprint_stays_bounded"
REAL_BINARY = "real_binary_supervising_escaping_churn_stays_bounded"
FOOTPRINT = "reference_host_footprint_measurements_write_raw_and_derived_json"
RUNTIME = "reference_host_runtime_measurements_write_raw_and_derived_json"
SCENARIOS = "reference_host_scenarios_write_reports_and_false_intervention_count"

FAKE_CARGO = """#!/bin/bash
# `build` succeeds at once. `test` writes the chunk artifact tagged with STUB_TAG (unless
# STUB_NO_ARTIFACT=1), then exits 101 when the test name equals STUB_FAIL.
set -eu
if [[ "$1" == build ]]; then exit 0; fi
name="" state=0
for argument in "$@"; do
    case $state in
        0) if [[ "$argument" == --test ]]; then state=1; fi ;;
        1) state=2 ;;
        2) name=$argument; break ;;
    esac
done
echo "stub chunk $name attempt $STUB_TAG"
if [[ "${STUB_NO_ARTIFACT:-0}" != 1 ]]; then
    if [[ -n "${MLX_GUARD_SCENARIO_OUTPUT_DIRECTORY:-}" ]]; then
        directory=$MLX_GUARD_SCENARIO_OUTPUT_DIRECTORY
        if [[ -e "$directory" ]]; then
            echo "scenario output directory already exists: $directory" >&2
            exit 101
        fi
        mkdir -p "$directory/reports"
        printf '{"attempt": "%s"}\\n' "$STUB_TAG" >"$directory/reports/scenario.json"
    fi
    for variable in MLX_GUARD_SOAK_OUTPUT MLX_GUARD_SOAK_BINARY_OUTPUT \\
        MLX_GUARD_ENDURANCE_OUTPUT MLX_GUARD_CHURN_OUTPUT MLX_GUARD_CALIBRATION_OUTPUT \\
        MLX_GUARD_RUNTIME_CALIBRATION_OUTPUT MLX_GUARD_INTERVENTION_OUTPUT \\
        MLX_GUARD_ENVELOPE_OUTPUT; do
        path=${!variable:-}
        if [[ -n "$path" ]]; then
            printf '{"requested_duration_seconds": 1, "attempt": "%s"}\\n' "$STUB_TAG" >"$path"
        fi
    done
    # The real-binary chunk also has the supervisor write its own report.
    if [[ -n "${MLX_GUARD_SOAK_BINARY_REPORT:-}" ]]; then
        printf '{"attempt": "%s"}\\n' "$STUB_TAG" >"$MLX_GUARD_SOAK_BINARY_REPORT"
    fi
fi
if [[ "$name" == "${STUB_FAIL:-}" ]]; then
    echo "stub chunk $name failed its bound"
    exit 101
fi
"""

FAKE_COMMANDS = {
    "cargo": FAKE_CARGO,
    "git": '#!/bin/bash\nif [[ "$1" == rev-parse ]]; then echo 0123456789abcdef; fi\n',
    "system_profiler": (
        "#!/bin/bash\n"
        'if [[ "$1" == -json ]]; then echo "{}"; exit 0; fi\n'
        'echo "      Chip: Apple M1 Max"\n'
        'echo "      Memory: 32 GB"\n'
    ),
    "uname": (
        '#!/bin/bash\ncase "${1:-}" in -m) echo arm64 ;; -r) echo 1.0 ;; *) echo Darwin ;; esac\n'
    ),
    "rustc": "#!/bin/bash\necho rustc 0.0.0\n",
    "sw_vers": "#!/bin/bash\necho ProductVersion: 0.0\n",
    "scutil": "#!/bin/bash\necho host\n",
    # An empty process table: no stray fixture, from this run or from anything else on the host.
    "ps": "#!/bin/bash\necho '  PID  PPID ELAPSED COMMAND'\n",
    "sleep": "#!/bin/bash\nexit 0\n",
}

FAKE_HELPERS = {
    "host-profile-label.sh": "#!/bin/bash\ncat >/dev/null\necho test-host\n",
    "hardware-facts.sh": (
        '#!/bin/bash\ncat >/dev/null\nif [[ "$1" == sanitized ]]; then echo "Chip: Test"; fi\n'
    ),
    "build-metal-calibration-fixture.sh": "#!/bin/bash\nexit 0\n",
}


def write_executable(path: Path, text: str) -> None:
    """Write TEXT to PATH and make it executable."""
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


class ScratchCapture:
    """One capture script copied into a scratch repository with stand-in tools."""

    def __init__(self, root: Path, script: str) -> None:
        """Lay out `root/repo/scripts/<script>`, the stand-ins, and the output path."""
        self.repo = root / "repo"
        scripts = self.repo / "scripts"
        scripts.mkdir(parents=True)
        self.script = scripts / script
        shutil.copy2(SCRIPTS_DIRECTORY / script, self.script)
        # The real publish scan, so a run that passes every chunk reaches the published bundle.
        shutil.copy2(SCRIPTS_DIRECTORY / "scan-evidence-bundle.sh", scripts)
        for helper, text in FAKE_HELPERS.items():
            write_executable(scripts / helper, text)
        self.bin = root / "bin"
        self.bin.mkdir()
        for command, text in FAKE_COMMANDS.items():
            write_executable(self.bin / command, text)
        self.output = root / "bundle"
        self.published_logs = root / "bundle.logs"
        self.staging = root / "bundle.staging"
        self.logs = self.staging / "logs"
        self.attempts = self.logs / "attempts"

    def run(
        self, *, tag: str, fail: str, no_artifact: bool = False
    ) -> subprocess.CompletedProcess[str]:
        """Run the script once; the stand-in chunk named FAIL fails after writing its artifact."""
        env = {key: value for key, value in os.environ.items() if not key.startswith("MLX_GUARD_")}
        env["PATH"] = f"{self.bin}{os.pathsep}{env.get('PATH', '')}"
        env["MLX_GUARD_SOAK_REFERENCE_SECONDS"] = "1"
        env["MLX_GUARD_CALIBRATION_ENDURANCE_SECONDS"] = "1"
        env["STUB_TAG"] = tag
        env["STUB_FAIL"] = fail
        env["STUB_NO_ARTIFACT"] = "1" if no_artifact else "0"
        # The system bash (3.2 on macOS) is the interpreter the scripts target, whatever comes
        # first on PATH.
        return subprocess.run(
            ["/bin/bash", str(self.script), str(self.output)],
            capture_output=True,
            text=True,
            check=False,
            env=env,
            cwd=self.repo,
            timeout=60,
        )

    def attempt_names(self) -> list[str]:
        """Every path under the staging directory whose name marks an earlier attempt."""
        return sorted(
            str(path.relative_to(self.staging))
            for path in self.staging.rglob("*")
            if ".attempt-" in path.name
        )


def attempt_names_under(directory: Path) -> list[str]:
    """Every path under DIRECTORY whose name marks an earlier attempt."""
    return sorted(
        str(path.relative_to(directory))
        for path in directory.rglob("*")
        if ".attempt-" in path.name
    )


class RerunKeepsFailedChunkTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red."""

    def scratch(self, script: str) -> ScratchCapture:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        return ScratchCapture(Path(directory.name), script)

    def run_failing(
        self, capture: ScratchCapture, *, tag: str, fail: str, no_artifact: bool = False
    ) -> subprocess.CompletedProcess[str]:
        # The failing `cargo test` exit (101) ends the script through `set -e` and `pipefail`.
        completed = capture.run(tag=tag, fail=fail, no_artifact=no_artifact)
        self.assertEqual(completed.returncode, 101, completed.stdout + completed.stderr)
        return completed

    def run_passing(self, capture: ScratchCapture, *, tag: str) -> None:
        completed = capture.run(tag=tag, fail="no chunk fails")
        self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)

    # soak-reference-host.sh

    def test_soak_rerun_keeps_failed_chunk_as_attempt_one(self) -> None:
        # Red if run_chunk goes back to `rm -f "$artifact"` (the failed JSON is gone) or lets
        # `tee` overwrite the old transcript; red too if a first run already moves something.
        capture = self.scratch(SOAK)
        self.run_failing(capture, tag="first", fail=ESCAPING_CHURN)
        self.assertEqual(capture.attempt_names(), [])
        self.run_failing(capture, tag="second", fail=ESCAPING_CHURN)
        self.assertIn('"first"', (capture.attempts / "escaping-churn.attempt-1.json").read_text())
        self.assertIn('"second"', (capture.staging / "escaping-churn.json").read_text())
        kept_log = (capture.attempts / "escaping-churn.attempt-1.log").read_text()
        self.assertIn("attempt first", kept_log)
        latest_log = (capture.logs / "escaping-churn.log").read_text()
        self.assertIn("attempt second", latest_log)
        self.assertNotIn("attempt first", latest_log)

    def test_soak_second_failed_rerun_adds_attempt_two_without_touching_attempt_one(self) -> None:
        # Red if the attempt number is fixed at 1 or does not skip a taken number: the second
        # rerun would overwrite (or nest into) attempt 1 instead of writing attempt 2.
        capture = self.scratch(SOAK)
        self.run_failing(capture, tag="first", fail=ESCAPING_CHURN)
        self.run_failing(capture, tag="second", fail=ESCAPING_CHURN)
        kept = capture.attempts / "escaping-churn.attempt-1.json"
        before = kept.stat()
        self.run_failing(capture, tag="third", fail=ESCAPING_CHURN)
        after = kept.stat()
        self.assertEqual((after.st_ino, after.st_mtime_ns), (before.st_ino, before.st_mtime_ns))
        self.assertIn('"first"', kept.read_text())
        self.assertIn('"second"', (capture.attempts / "escaping-churn.attempt-2.json").read_text())
        self.assertIn('"third"', (capture.staging / "escaping-churn.json").read_text())
        self.assertIn(
            "attempt second", (capture.attempts / "escaping-churn.attempt-2.log").read_text()
        )
        self.assertIn(
            "attempt first", (capture.attempts / "escaping-churn.attempt-1.log").read_text()
        )

    def test_soak_no_artifact_then_two_failures_keep_every_transcript(self) -> None:
        # Red if the free-number check ignores the transcripts: after a chunk that wrote no JSON,
        # attempt 1 holds only a transcript, and the next keep would reuse number 1 and
        # overwrite that first transcript. Red too if only an existing artifact triggers a move.
        capture = self.scratch(SOAK)
        self.run_failing(capture, tag="first", fail=ESCAPING_CHURN, no_artifact=True)
        self.run_failing(capture, tag="second", fail=ESCAPING_CHURN)
        self.run_failing(capture, tag="third", fail=ESCAPING_CHURN)
        self.assertEqual(
            capture.attempt_names(),
            [
                "logs/attempts/escaping-churn.attempt-1.log",
                "logs/attempts/escaping-churn.attempt-2.json",
                "logs/attempts/escaping-churn.attempt-2.log",
            ],
        )
        self.assertIn(
            "attempt first", (capture.attempts / "escaping-churn.attempt-1.log").read_text()
        )
        self.assertIn(
            "attempt second", (capture.attempts / "escaping-churn.attempt-2.log").read_text()
        )
        self.assertIn('"second"', (capture.attempts / "escaping-churn.attempt-2.json").read_text())

    def test_soak_rerun_keeps_real_binary_supervisor_report_with_its_attempt(self) -> None:
        # Red if the real-binary chunk's supervisor report is left for the rerun to overwrite, or
        # is kept under a different attempt number than that chunk's JSON and transcript.
        capture = self.scratch(SOAK)
        self.run_failing(capture, tag="first", fail=REAL_BINARY)
        self.run_failing(capture, tag="second", fail=REAL_BINARY)
        kept = capture.attempts / "real-binary-report.attempt-1.json"
        self.assertIn('"first"', kept.read_text())
        self.assertIn('"second"', (capture.logs / "real-binary-report.json").read_text())
        self.assertIn('"first"', (capture.attempts / "real-binary.attempt-1.json").read_text())
        self.assertIn("attempt first", (capture.attempts / "real-binary.attempt-1.log").read_text())

    def test_soak_report_attempt_number_follows_the_chunk_past_one(self) -> None:
        # Red if the report's kept name does not use the shared attempt number (for example a
        # hard-coded `.attempt-1.json`): the second kept report would overwrite the first.
        capture = self.scratch(SOAK)
        for tag in ("first", "second", "third"):
            self.run_failing(capture, tag=tag, fail=REAL_BINARY)
        first = capture.attempts / "real-binary-report.attempt-1.json"
        second = capture.attempts / "real-binary-report.attempt-2.json"
        self.assertIn('"first"', first.read_text())
        self.assertIn('"second"', second.read_text())
        self.assertIn('"third"', (capture.logs / "real-binary-report.json").read_text())

    def test_soak_existing_report_attempt_is_never_overwritten(self) -> None:
        # Red if the free-number check ignores the kept reports: a report attempt already holding
        # number 1 (with no artifact or transcript of that number) would be overwritten.
        capture = self.scratch(SOAK)
        self.run_failing(capture, tag="first", fail=REAL_BINARY)
        capture.attempts.mkdir(parents=True)
        earlier = capture.attempts / "real-binary-report.attempt-1.json"
        earlier.write_text('{"attempt": "kept by hand"}\n')
        self.run_failing(capture, tag="second", fail=REAL_BINARY)
        self.assertIn("kept by hand", earlier.read_text())
        self.assertIn(
            '"first"', (capture.attempts / "real-binary-report.attempt-2.json").read_text()
        )
        self.assertIn('"first"', (capture.attempts / "real-binary.attempt-2.json").read_text())

    def test_soak_passed_chunk_is_skipped_and_nothing_of_it_is_moved(self) -> None:
        # Red if the move runs before the skip check: a passed chunk would be renamed to an
        # attempt and rerun, or its transcript moved aside although it is the evidence of a pass.
        capture = self.scratch(SOAK)
        self.run_failing(capture, tag="first", fail=REAL_BINARY)
        completed = self.run_failing(capture, tag="second", fail=REAL_BINARY)
        self.assertIn("[skip] escaping-churn", completed.stdout)
        self.assertIn('"first"', (capture.staging / "escaping-churn.json").read_text())
        self.assertIn("attempt first", (capture.logs / "escaping-churn.log").read_text())
        self.assertTrue((capture.logs / "escaping-churn.ok").exists())
        self.assertNotIn(
            "escaping-churn", " ".join(capture.attempt_names()), capture.attempt_names()
        )

    def test_soak_stale_success_sentinel_is_cleared_on_rerun(self) -> None:
        # Red if the rerun leaves the old `.ok` in place: once the rerun fails and writes its
        # JSON, the next run would skip the chunk as passed and publish a failed artifact.
        capture = self.scratch(SOAK)
        self.run_failing(capture, tag="first", fail=REAL_BINARY)
        aside = capture.staging.parent / "moved-aside.json"
        shutil.move(capture.staging / "escaping-churn.json", aside)
        self.run_failing(capture, tag="second", fail=ESCAPING_CHURN)
        completed = self.run_failing(capture, tag="third", fail=ESCAPING_CHURN)
        self.assertNotIn("[skip] escaping-churn", completed.stdout)
        self.assertIn('"second"', (capture.attempts / "escaping-churn.attempt-2.json").read_text())

    def test_soak_fail_then_pass_publishes_no_attempt_and_keeps_it_with_the_logs(self) -> None:
        # Red if attempts sit where the publish step picks them up (the staging root, whose
        # `*.json` is moved into the bundle), or if they are dropped instead of leaving with the
        # transcripts.
        capture = self.scratch(SOAK)
        self.run_failing(capture, tag="first", fail=ESCAPING_CHURN)
        self.run_passing(capture, tag="second")
        self.assertEqual(attempt_names_under(capture.output), [])
        self.assertIn('"second"', (capture.output / "escaping-churn.json").read_text())
        kept = capture.published_logs / "attempts"
        self.assertIn('"first"', (kept / "escaping-churn.attempt-1.json").read_text())
        self.assertIn("attempt first", (kept / "escaping-churn.attempt-1.log").read_text())

    # calibrate-host.sh

    def test_calibrate_rerun_keeps_failed_chunk_as_attempt_one(self) -> None:
        # Red if run_chunk goes back to `rm -rf "$artifact"` or lets `tee` overwrite the old
        # transcript; red too if a first run already moves something.
        capture = self.scratch(CALIBRATE)
        self.run_failing(capture, tag="first", fail=FOOTPRINT)
        self.assertEqual(capture.attempt_names(), [])
        self.run_failing(capture, tag="second", fail=FOOTPRINT)
        self.assertIn('"first"', (capture.attempts / "footprint.attempt-1.json").read_text())
        self.assertIn('"second"', (capture.staging / "footprint.json").read_text())
        self.assertIn("attempt first", (capture.attempts / "footprint.attempt-1.log").read_text())
        latest_log = (capture.logs / "footprint.log").read_text()
        self.assertIn("attempt second", latest_log)
        self.assertNotIn("attempt first", latest_log)

    def test_calibrate_second_failed_rerun_adds_attempt_two_without_touching_attempt_one(
        self,
    ) -> None:
        # Red if the attempt number is fixed at 1 or does not skip a taken number.
        capture = self.scratch(CALIBRATE)
        self.run_failing(capture, tag="first", fail=FOOTPRINT)
        self.run_failing(capture, tag="second", fail=FOOTPRINT)
        kept = capture.attempts / "footprint.attempt-1.json"
        before = kept.stat()
        self.run_failing(capture, tag="third", fail=FOOTPRINT)
        after = kept.stat()
        self.assertEqual((after.st_ino, after.st_mtime_ns), (before.st_ino, before.st_mtime_ns))
        self.assertIn('"first"', kept.read_text())
        self.assertIn('"second"', (capture.attempts / "footprint.attempt-2.json").read_text())
        self.assertIn('"third"', (capture.staging / "footprint.json").read_text())
        self.assertIn("attempt second", (capture.attempts / "footprint.attempt-2.log").read_text())
        self.assertIn("attempt first", (capture.attempts / "footprint.attempt-1.log").read_text())

    def test_calibrate_rerun_keeps_failed_scenarios_directory_as_attempt_one(self) -> None:
        # Red if the scenarios directory is removed (`rm -rf`) or renamed with a `.json`-style
        # suffix rule that does not fit a directory: its reports must survive whole as
        # `scenarios.attempt-1/`, and the rerun must get an empty canonical directory.
        capture = self.scratch(CALIBRATE)
        self.run_failing(capture, tag="first", fail=SCENARIOS)
        self.run_failing(capture, tag="second", fail=SCENARIOS)
        kept = capture.attempts / "scenarios.attempt-1" / "reports" / "scenario.json"
        self.assertIn('"first"', kept.read_text())
        latest = capture.staging / "scenarios" / "reports" / "scenario.json"
        self.assertIn('"second"', latest.read_text())
        self.assertIn("attempt first", (capture.attempts / "scenarios.attempt-1.log").read_text())

    def test_calibrate_no_artifact_then_two_failures_keep_every_transcript(self) -> None:
        # Red if the free-number check ignores the transcripts (the third run would overwrite the
        # first transcript as attempt 1), or if only an existing artifact triggers a move.
        capture = self.scratch(CALIBRATE)
        self.run_failing(capture, tag="first", fail=FOOTPRINT, no_artifact=True)
        self.run_failing(capture, tag="second", fail=FOOTPRINT)
        self.run_failing(capture, tag="third", fail=FOOTPRINT)
        self.assertEqual(
            capture.attempt_names(),
            [
                "logs/attempts/footprint.attempt-1.log",
                "logs/attempts/footprint.attempt-2.json",
                "logs/attempts/footprint.attempt-2.log",
            ],
        )
        self.assertIn("attempt first", (capture.attempts / "footprint.attempt-1.log").read_text())
        self.assertIn("attempt second", (capture.attempts / "footprint.attempt-2.log").read_text())
        self.assertIn('"second"', (capture.attempts / "footprint.attempt-2.json").read_text())

    def test_calibrate_passed_chunk_is_skipped_and_nothing_of_it_is_moved(self) -> None:
        # Red if the move runs before the skip check: a passed chunk would be renamed and rerun.
        capture = self.scratch(CALIBRATE)
        self.run_failing(capture, tag="first", fail=RUNTIME)
        completed = self.run_failing(capture, tag="second", fail=RUNTIME)
        self.assertIn("[skip] footprint", completed.stdout)
        self.assertIn('"first"', (capture.staging / "footprint.json").read_text())
        self.assertIn("attempt first", (capture.logs / "footprint.log").read_text())
        self.assertTrue((capture.logs / "footprint.ok").exists())
        self.assertNotIn("footprint", " ".join(capture.attempt_names()), capture.attempt_names())

    def test_calibrate_stale_success_sentinel_is_cleared_on_rerun(self) -> None:
        # Red if the rerun leaves the old `.ok` in place (`rm -f "$log_directory/$name.ok"`
        # deleted): once the rerun fails and writes its JSON, the next run would skip the chunk
        # as passed and publish a failed artifact.
        capture = self.scratch(CALIBRATE)
        self.run_failing(capture, tag="first", fail=RUNTIME)
        aside = capture.staging.parent / "moved-aside.json"
        shutil.move(capture.staging / "footprint.json", aside)
        self.run_failing(capture, tag="second", fail=FOOTPRINT)
        completed = self.run_failing(capture, tag="third", fail=FOOTPRINT)
        self.assertNotIn("[skip] footprint", completed.stdout)
        self.assertIn('"second"', (capture.attempts / "footprint.attempt-2.json").read_text())

    def test_calibrate_fail_then_pass_publishes_no_attempt_and_keeps_it_with_the_logs(
        self,
    ) -> None:
        # Red if attempts sit inside the tree that becomes the bundle (the staging root, which
        # is moved whole into the output), or if they are dropped instead of leaving with the
        # transcripts.
        capture = self.scratch(CALIBRATE)
        self.run_failing(capture, tag="first", fail=FOOTPRINT)
        self.run_failing(capture, tag="second", fail=SCENARIOS)
        self.run_passing(capture, tag="third")
        self.assertEqual(attempt_names_under(capture.output), [])
        self.assertIn('"second"', (capture.output / "footprint.json").read_text())
        published_scenario = capture.output / "scenarios" / "reports" / "scenario.json"
        self.assertIn('"third"', published_scenario.read_text())
        kept = capture.published_logs / "attempts"
        self.assertIn('"first"', (kept / "footprint.attempt-1.json").read_text())
        self.assertIn("attempt first", (kept / "footprint.attempt-1.log").read_text())
        scenario = kept / "scenarios.attempt-1" / "reports" / "scenario.json"
        self.assertIn('"second"', scenario.read_text())


if __name__ == "__main__":
    unittest.main()
