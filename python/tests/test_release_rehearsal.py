"""Tests that CI's cold release rehearsal runs the release build's steps up to its test step."""

from __future__ import annotations

import re
import unittest
from pathlib import Path

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
ROOT = Path(__file__).resolve().parents[2]
CI = ROOT / ".github/workflows/ci.yml"
RELEASE = ROOT / ".github/workflows/release.yml"

RELEASE_JOB = "build"
REHEARSAL_JOB = "release-rehearsal"
# The last release step the rehearsal runs.
TEST_STEP_RUN = "run: ./scripts/test-full.sh"
# Release steps before the test step that the rehearsal leaves out, by their `name:` line. The
# tag check reads the pushed tag and needs the commit to be on main already, so it cannot run on
# a pull request; it builds and tests nothing.
UNREHEARSED_STEP_NAMES = frozenset({"name: Verify tag, version, and provenance"})
# Job-level settings that change where or how the steps run.
JOB_SETTINGS = ("runs-on", "env", "defaults", "container", "shell")
# Workflow-level settings that reach every job in the file, so they must match between the two.
WORKFLOW_SETTINGS = ("env", "defaults")
# Job keys the rehearsal must not have: either lets a failing run report green or splits it.
FORBIDDEN_REHEARSAL_KEYS = ("continue-on-error", "strategy")
# The rehearsal runs on every code change, like the Python job: never on a docs-only change.
SCOPE_GATE = "${{ !cancelled() && needs.scope.outputs.scope != 'docs' }}"
# A cost cap on a hung run. Pinned exactly: a higher one holds a macOS runner longer, and a lower
# one (a cold run takes about 7 minutes) fails the job on time instead of on a test.
REHEARSAL_TIMEOUT_MINUTES = "45"

_TOP_KEY = re.compile(r"^([A-Za-z_][\w-]*):\s*(.*)$")
_JOB = re.compile(r"^  ([A-Za-z_][\w-]*):\s*$")
_JOB_KEY = re.compile(r"^    ([A-Za-z_][\w-]*):\s*(.*)$")
_STEP_START = re.compile(r"^      - ")

_SAMPLE = """\
jobs:
  build:
    runs-on: macos-15
    steps:
      - uses: actions/checkout@abc # v1
      - name: Verify tag, version, and provenance
        run: test -n "$GITHUB_REF_NAME"
      # A comment between steps.
      - name: Run the full test suite
        run: ./scripts/test-full.sh
        env:
          RUST_TEST_THREADS: "1"
      - run: ./scripts/build-release.sh dist
  release-rehearsal:
    runs-on: macos-15
    if: ${{ !cancelled() && needs.scope.outputs.scope != 'docs' }}
    timeout-minutes: 45
    steps:
      - uses: actions/checkout@abc # v1
      - name: Run the full test suite
        run: ./scripts/test-full.sh
        env:
          RUST_TEST_THREADS: "1"
"""
_REHEARSAL_HEAD = "  release-rehearsal:\n    runs-on: macos-15\n"
_REHEARSAL_GATE = "    if: ${{ !cancelled() && needs.scope.outputs.scope != 'docs' }}\n"
_REHEARSAL_TIMEOUT = "    timeout-minutes: 45\n"


def job_body(workflow: str, job: str) -> list[str]:
    """Return the lines of one job under `jobs:`, without the job's own key line."""
    body: list[str] = []
    current: str | None = None
    in_jobs = False
    for line in workflow.splitlines():
        if line.rstrip() == "jobs:":
            in_jobs = True
            continue
        if not in_jobs:
            continue
        started = _JOB.match(line)
        if started:
            current = started.group(1)
            continue
        if current == job:
            body.append(line)
    return body


def workflow_settings(workflow: str) -> dict[str, str]:
    """Return the workflow-level settings in `WORKFLOW_SETTINGS`, nested lines joined."""
    settings: dict[str, str] = {}
    key: str | None = None
    for line in workflow.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        found = _TOP_KEY.match(line)
        if found:
            key = found.group(1) if found.group(1) in WORKFLOW_SETTINGS else None
            if key is not None:
                settings[key] = found.group(2).strip()
        elif key is not None and line.startswith(" "):
            settings[key] += "\n" + line.strip()
    return settings


def job_keys(workflow: str, job: str) -> dict[str, str]:
    """Return every job-level key of one job, each with its nested lines joined."""
    keys: dict[str, str] = {}
    key: str | None = None
    for line in job_body(workflow, job):
        found = _JOB_KEY.match(line)
        if found:
            key = found.group(1)
            keys[key] = found.group(2).strip()
        elif key is not None and line.startswith("      "):
            keys[key] += "\n" + line.strip()
    return keys


def job_settings(workflow: str, job: str) -> dict[str, str]:
    """Return the job-level settings in `JOB_SETTINGS`, each with its nested lines joined."""
    return {k: v for k, v in job_keys(workflow, job).items() if k in JOB_SETTINGS}


def rehearsal_guard(ci: str) -> list[str]:
    """Return the rehearsal job's settings that could silence it or change when it runs."""
    keys = job_keys(ci, REHEARSAL_JOB)
    problems = [f"rehearsal sets `{key}`" for key in FORBIDDEN_REHEARSAL_KEYS if key in keys]
    if keys.get("if") != SCOPE_GATE:
        problems.append(f"rehearsal `if:` is {keys.get('if')!r}, not {SCOPE_GATE!r}")
    if keys.get("timeout-minutes") != REHEARSAL_TIMEOUT_MINUTES:
        problems.append(
            f"rehearsal `timeout-minutes` is {keys.get('timeout-minutes')!r}, "
            f"not {REHEARSAL_TIMEOUT_MINUTES}"
        )
    return problems


def steps(workflow: str, job: str) -> list[tuple[str, ...]]:
    """Return each step of a job as its lines, trailing space and comment-only lines dropped."""
    found: list[list[str]] = []
    in_steps = False
    for line in job_body(workflow, job):
        if line.rstrip() == "    steps:":
            in_steps = True
            continue
        if not in_steps or not line.strip() or line.strip().startswith("#"):
            continue
        if not line.startswith("      "):
            in_steps = False
        elif _STEP_START.match(line):
            found.append([line.rstrip()])
        elif found:
            found[-1].append(line.rstrip())
    return [tuple(step) for step in found]


def rehearsed_release_steps(release: str) -> list[tuple[str, ...]]:
    """Return the release build's steps up to and including the test step, as rehearsed."""
    rehearsed: list[tuple[str, ...]] = []
    for step in steps(release, RELEASE_JOB):
        if not any(line.strip().lstrip("- ") in UNREHEARSED_STEP_NAMES for line in step):
            rehearsed.append(step)
        if any(line.strip().lstrip("- ") == TEST_STEP_RUN for line in step):
            return rehearsed
    raise AssertionError(f"no `{TEST_STEP_RUN}` step in the release job `{RELEASE_JOB}`")


def drift(ci: str, release: str) -> list[str]:
    """Return how the rehearsal differs from the release build, or an empty list."""
    problems: list[str] = []
    expected = rehearsed_release_steps(release)
    actual = steps(ci, REHEARSAL_JOB)
    if len(actual) != len(expected):
        problems.append(f"release runs {len(expected)} steps, the rehearsal {len(actual)}")
    for number, (want, got) in enumerate(zip(expected, actual, strict=False), start=1):
        if want != got:
            problems.append(f"step {number}: release {list(want)}, rehearsal {list(got)}")
    if job_settings(ci, REHEARSAL_JOB) != job_settings(release, RELEASE_JOB):
        problems.append(
            f"job settings differ: release {job_settings(release, RELEASE_JOB)}, "
            f"rehearsal {job_settings(ci, REHEARSAL_JOB)}"
        )
    if workflow_settings(ci) != workflow_settings(release):
        problems.append(
            f"workflow settings differ: release {workflow_settings(release)}, "
            f"ci {workflow_settings(ci)}"
        )
    problems.extend(rehearsal_guard(ci))
    return problems


class ReleaseRehearsalTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red."""

    def test_rehearsal_matches_the_release_build_up_to_the_test_step(self) -> None:
        # Red if a pin, a `with:` input, a run line, the test step's env, the runner image or the
        # step order differs between ci.yml's rehearsal and release.yml's build job, if the two
        # files' workflow-wide `env:` or `defaults:` differ, or if the rehearsal's `if:`, timeout,
        # `continue-on-error` or `strategy` could silence it or change when it runs.
        self.assertEqual(
            drift(CI.read_text(encoding="utf-8"), RELEASE.read_text(encoding="utf-8")), []
        )

    def test_sample_rehearsal_drops_only_the_tag_check_and_stops_at_the_test_step(self) -> None:
        # Red if the reader keeps the tag check, drops a rehearsed step, or reads past the test
        # step into the build steps the rehearsal does not run.
        self.assertEqual(drift(_SAMPLE, _SAMPLE), [])
        self.assertEqual(
            rehearsed_release_steps(_SAMPLE),
            [
                ("      - uses: actions/checkout@abc # v1",),
                (
                    "      - name: Run the full test suite",
                    "        run: ./scripts/test-full.sh",
                    "        env:",
                    '          RUST_TEST_THREADS: "1"',
                ),
            ],
        )

    def test_a_changed_pin_in_the_rehearsal_is_reported(self) -> None:
        # Red if the comparison ignores step bodies, which would make the first test vacuous.
        ci = CI.read_text(encoding="utf-8")
        line = "        run: cargo install cargo-audit --locked --version 0.22.1\n"
        bumped = ci.replace(line, line.replace("0.22.1", "0.22.2"))
        self.assertNotEqual(bumped, ci)
        self.assertNotEqual(drift(bumped, RELEASE.read_text(encoding="utf-8")), [])

    def test_a_changed_thread_count_in_the_rehearsal_test_step_is_reported(self) -> None:
        # Red if the reader stops before the test step's `env:`, so the condition behind the
        # release failures could drift unseen.
        ci = CI.read_text(encoding="utf-8")
        changed = ci.replace('RUST_TEST_THREADS: "1"', 'RUST_TEST_THREADS: "2"')
        self.assertNotEqual(changed, ci)
        self.assertNotEqual(drift(changed, RELEASE.read_text(encoding="utf-8")), [])

    def test_a_release_step_added_before_the_test_step_is_reported(self) -> None:
        # Red if the comparison only checks that the rehearsal's steps appear in the release job.
        release = RELEASE.read_text(encoding="utf-8")
        anchor = "      - name: Run the full test suite\n"
        extended = release.replace(anchor, "      - run: ./scripts/warm-up.sh\n" + anchor)
        self.assertNotEqual(extended, release)
        self.assertNotEqual(drift(CI.read_text(encoding="utf-8"), extended), [])

    def test_a_different_runner_image_for_the_rehearsal_is_reported(self) -> None:
        # Red if job-level settings are not compared: macos-14 or a larger runner would rehearse
        # nothing about the release runner.
        sample = _SAMPLE.replace(
            "  release-rehearsal:\n    runs-on: macos-15\n",
            "  release-rehearsal:\n    runs-on: macos-15-xlarge\n",
        )
        self.assertNotEqual(sample, _SAMPLE)
        self.assertNotEqual(drift(sample, _SAMPLE), [])

    def test_a_job_level_env_on_the_rehearsal_is_reported(self) -> None:
        # Red if the settings reader skips nested lines or `env`, so a job-wide override such as
        # `CARGO_BUILD_JOBS` could change the rehearsal's conditions unseen.
        sample = _SAMPLE.replace(
            "  release-rehearsal:\n    runs-on: macos-15\n",
            "  release-rehearsal:\n    runs-on: macos-15\n    env:\n      CARGO_BUILD_JOBS: 1\n",
        )
        self.assertNotEqual(sample, _SAMPLE)
        self.assertNotEqual(drift(sample, _SAMPLE), [])
        # Both jobs set the variable, to different values: the nested value must be compared.
        both = sample.replace(
            "  build:\n    runs-on: macos-15\n",
            "  build:\n    runs-on: macos-15\n    env:\n      CARGO_BUILD_JOBS: 3\n",
        )
        self.assertNotEqual(both, sample)
        self.assertNotEqual(drift(both, both), [])

    def test_a_workflow_level_env_or_defaults_difference_is_reported(self) -> None:
        # Red if only job-level settings are compared: a workflow-wide `env:` or `defaults:` in
        # either file reaches every job in it, the rehearsal or the release build included.
        ci_env = "env:\n  CARGO_INCREMENTAL: 1\n" + _SAMPLE
        self.assertNotEqual(drift(ci_env, _SAMPLE), [])
        # The flow style puts the whole block on the key's own line.
        ci_flow = "env: {CARGO_INCREMENTAL: 1}\n" + _SAMPLE
        self.assertNotEqual(drift(ci_flow, _SAMPLE), [])
        release_defaults = "defaults:\n  run:\n    shell: bash\n" + _SAMPLE
        self.assertNotEqual(drift(_SAMPLE, release_defaults), [])
        # Both files set the variable, to different values: the nested value must be compared.
        release_env = "env:\n  CARGO_INCREMENTAL: 0\n" + _SAMPLE
        self.assertNotEqual(drift(ci_env, release_env), [])
        # The same block in both files is no drift.
        self.assertEqual(drift(ci_env, ci_env), [])

    def test_a_job_level_continue_on_error_on_the_rehearsal_is_reported(self) -> None:
        # Red if job keys outside the compared settings are ignored: a red run would report green.
        sample = _SAMPLE.replace(_REHEARSAL_HEAD, _REHEARSAL_HEAD + "    continue-on-error: true\n")
        self.assertNotEqual(sample, _SAMPLE)
        self.assertNotEqual(drift(sample, _SAMPLE), [])

    def test_a_strategy_on_the_rehearsal_is_reported(self) -> None:
        # Red if a `strategy:` passes: a matrix or `fail-fast` changes how many runs there are and
        # which of them count.
        sample = _SAMPLE.replace(
            _REHEARSAL_HEAD, _REHEARSAL_HEAD + "    strategy:\n      fail-fast: false\n"
        )
        self.assertNotEqual(sample, _SAMPLE)
        self.assertNotEqual(drift(sample, _SAMPLE), [])

    def test_a_rehearsal_condition_other_than_the_scope_gate_is_reported(self) -> None:
        # Red if the job's `if:` is not pinned: `if: false` would never run it and stay green.
        for gate in ("    if: false\n", "    if: ${{ !cancelled() }}\n", ""):
            with self.subTest(gate=gate):
                sample = _SAMPLE.replace(_REHEARSAL_GATE, gate)
                self.assertNotEqual(sample, _SAMPLE)
                self.assertNotEqual(drift(sample, _SAMPLE), [])

    def test_a_missing_or_changed_rehearsal_timeout_is_reported(self) -> None:
        # Red if the timeout is unchecked: none means a hung run holds a macOS runner for the
        # 360-minute default, and one below a cold run's length fails it on time, not on a test.
        for timeout in ("", "    timeout-minutes: 5\n", "    timeout-minutes: 46\n"):
            with self.subTest(timeout=timeout):
                sample = _SAMPLE.replace(_REHEARSAL_TIMEOUT, timeout)
                self.assertNotEqual(sample, _SAMPLE)
                self.assertNotEqual(drift(sample, _SAMPLE), [])

    def test_the_rehearsal_restores_no_rust_cache_or_actions_cache(self) -> None:
        # Red if the rehearsal restores a Rust build cache: the release build starts cold, and a
        # warm build is the one difference main CI already covers. setup-uv's cache inputs are
        # not read here; the step comparison holds that step equal to the release build's.
        body = "\n".join(job_body(CI.read_text(encoding="utf-8"), REHEARSAL_JOB))
        self.assertTrue(body, f"expected a `{REHEARSAL_JOB}` job in ci.yml")
        self.assertNotIn("rust-cache", body)
        self.assertNotIn("actions/cache", body)


if __name__ == "__main__":
    unittest.main()
