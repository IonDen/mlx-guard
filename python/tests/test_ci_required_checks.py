"""Tests that the CI workflow reports every required check name on a documentation-only change."""

from __future__ import annotations

import re
import unittest
from pathlib import Path

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml"

# The only job-level conditions a matrix-named job may carry. Anything else can skip the job, and
# GitHub does not expand the matrix of a skipped job: it reports one check under the literal
# template name, so the required expanded names (for example `Rust (macos-15)`) never report and
# the pull request can never merge. Gate the steps instead, and write the condition on one line.
ALLOWED_JOB_CONDITIONS = frozenset({"", "${{ !cancelled() }}"})

# main's branch protection requires exactly these check names. Rename a job or change the matrix
# `os` list only together with the protection rule: a name that no longer reports stays
# "Expected" forever and no pull request can merge, including the one that renamed it.
REQUIRED_CHECK_NAMES = frozenset(
    {
        "Change scope",
        "Dependency policy",
        "Python package and wheel",
        "Rust (macos-15)",
        "Rust (ubuntu-24.04)",
    }
)
SCOPE_OUTPUT = "needs.scope.outputs.scope"

_JOB = re.compile(r"^  ([A-Za-z_][\w-]*):\s*$")
_JOB_KEY = re.compile(r"^    (name|if):\s*(.*)$")
_MATRIX_OS = re.compile(r"^        os:\s*\[(.*)\]\s*$")
_MATRIX_EXTRA = re.compile(r"^        (include|exclude):")
_STEP_START = re.compile(r"^      - ")
_STEP_IF = re.compile(r"^(?:      - |        )if:\s*(.*)$")

_GATED_STEPS = """\
name: CI
jobs:
  rust:
    name: Rust (${{ matrix.os }})
    needs: scope
    if: ${{ !cancelled() }}
    steps:
      - if: ${{ needs.scope.outputs.scope != 'docs' }}
        run: cargo test
"""


def job_level_keys(workflow: str) -> dict[str, dict[str, str]]:
    """Return each job's own `name` and `if` values, ignoring anything nested under steps."""
    jobs: dict[str, dict[str, str]] = {}
    current: dict[str, str] | None = None
    in_jobs = False
    for line in workflow.splitlines():
        if line.rstrip() == "jobs:":
            in_jobs = True
            continue
        if not in_jobs:
            continue
        started = _JOB.match(line)
        if started:
            current = jobs.setdefault(started.group(1), {})
            continue
        key = _JOB_KEY.match(line)
        if key and current is not None:
            current[key.group(1)] = key.group(2).strip()
    return jobs


def job_bodies(workflow: str) -> dict[str, list[str]]:
    """Return the lines that belong to each job, in order."""
    bodies: dict[str, list[str]] = {}
    current: list[str] | None = None
    in_jobs = False
    for line in workflow.splitlines():
        if line.rstrip() == "jobs:":
            in_jobs = True
            continue
        if not in_jobs:
            continue
        started = _JOB.match(line)
        if started:
            current = bodies.setdefault(started.group(1), [])
            continue
        if current is not None:
            current.append(line)
    return bodies


def reported_check_names(workflow: str) -> set[str]:
    """Return the check names the workflow reports, with `${{ matrix.os }}` expanded."""
    names: set[str] = set()
    keys = job_level_keys(workflow)
    for job, body in job_bodies(workflow).items():
        name = keys[job].get("name", job)
        systems = [
            entry.strip()
            for line in body
            for found in [_MATRIX_OS.match(line)]
            if found
            for entry in found.group(1).split(",")
        ]
        if "${{ matrix.os }}" in name:
            names.update(name.replace("${{ matrix.os }}", system) for system in systems)
        else:
            names.add(name)
    return names


def unread_matrix_keys(workflow: str) -> list[str]:
    """Return `job:key` for matrix keys that add or drop check names this reader cannot follow."""
    return sorted(
        f"{job}:{found.group(1)}"
        for job, body in job_bodies(workflow).items()
        for line in body
        for found in [_MATRIX_EXTRA.match(line)]
        if found
    )


def ungated_steps(workflow: str) -> list[str]:
    """Return `job:step-number` for matrix-named job steps that do not read the change scope."""
    missing: list[str] = []
    for job in matrix_named_jobs(workflow):
        steps: list[list[str]] = []
        for line in job_bodies(workflow)[job]:
            if _STEP_START.match(line):
                steps.append([line])
            elif steps:
                steps[-1].append(line)
        for number, step in enumerate(steps, start=1):
            matches = [_STEP_IF.match(line) for line in step]
            conditions = [found.group(1) for found in matches if found]
            if not any(SCOPE_OUTPUT in condition for condition in conditions):
                missing.append(f"{job}:{number}")
    return missing


def matrix_named_jobs(workflow: str) -> dict[str, dict[str, str]]:
    """Return the jobs whose check name is built from the matrix."""
    return {
        job: keys
        for job, keys in job_level_keys(workflow).items()
        if "${{ matrix." in keys.get("name", "")
    }


def skippable_matrix_jobs(workflow: str) -> list[str]:
    """Return matrix-named jobs whose job-level `if` is anything but an allowed condition."""
    return sorted(
        job
        for job, keys in matrix_named_jobs(workflow).items()
        if keys.get("if", "") not in ALLOWED_JOB_CONDITIONS
    )


class RequiredCheckNameTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red."""

    def test_parser_reads_job_level_keys_and_not_step_conditions(self) -> None:
        # Red if the parser mistakes a step-level `if:` (six spaces or more) for a job-level one,
        # which would make the workflow assertion below fail on a correctly gated workflow.
        sample = (
            "name: CI\n"
            "jobs:\n"
            "  build:\n"
            "    name: Build (${{ matrix.os }})\n"
            "    steps:\n"
            "      - if: ${{ needs.scope.outputs.scope != 'docs' }}\n"
            "        run: make\n"
            "  lint:\n"
            "    name: Lint\n"
            "    if: ${{ needs.scope.outputs.scope != 'docs' }}\n"
        )
        self.assertEqual(
            job_level_keys(sample),
            {
                "build": {"name": "Build (${{ matrix.os }})"},
                "lint": {"name": "Lint", "if": "${{ needs.scope.outputs.scope != 'docs' }}"},
            },
        )

    def test_step_gated_matrix_job_is_accepted(self) -> None:
        # Red if the allowlist rejects the shape the workflow is supposed to have.
        self.assertEqual(skippable_matrix_jobs(_GATED_STEPS), [])

    def test_job_level_scope_gate_on_a_matrix_job_is_flagged(self) -> None:
        # Red if the original defect comes back: the matrix job skipped by the change scope.
        broken = _GATED_STEPS.replace(
            "    if: ${{ !cancelled() }}\n",
            "    if: ${{ !cancelled() && needs.scope.outputs.scope != 'docs' }}\n",
        )
        self.assertEqual(skippable_matrix_jobs(broken), ["rust"])

    def test_block_scalar_condition_on_a_matrix_job_is_flagged(self) -> None:
        # Red if a folded `if: >-` hides the same gate from a single-line reader.
        broken = _GATED_STEPS.replace(
            "    if: ${{ !cancelled() }}\n",
            "    if: >-\n      ${{ !cancelled() &&\n      needs.scope.outputs.scope != 'docs' }}\n",
        )
        self.assertEqual(skippable_matrix_jobs(broken), ["rust"])

    def test_renamed_gate_on_a_matrix_job_is_flagged(self) -> None:
        # Red if the check only looks for the word "scope": a renamed output skips the job too.
        broken = _GATED_STEPS.replace(
            "    if: ${{ !cancelled() }}\n",
            "    if: ${{ needs.classify.outputs.docs_only != 'true' }}\n",
        )
        self.assertEqual(skippable_matrix_jobs(broken), ["rust"])

    def test_ci_workflow_never_skips_a_matrix_named_job(self) -> None:
        # Red if ci.yml gates a matrix-named job at job level, by the change scope or anything
        # else: its required check names would stop reporting on the pull requests that skip it.
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertTrue(matrix_named_jobs(workflow), "expected a matrix-named job in ci.yml")
        self.assertEqual(skippable_matrix_jobs(workflow), [])

    def test_ci_workflow_reports_exactly_the_required_check_names(self) -> None:
        # Red if a job is renamed or the matrix `os` list changes without the protection rule.
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertEqual(reported_check_names(workflow), set(REQUIRED_CHECK_NAMES))

    def test_ci_workflow_matrix_uses_only_keys_the_name_reader_follows(self) -> None:
        # Red if the matrix gains `include:` or `exclude:`. Those add or drop check names that
        # `reported_check_names` does not read, so the required-names test above would stay green
        # while GitHub reports a different set. Teach the reader first, then use the key.
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertEqual(unread_matrix_keys(workflow), [])

    def test_a_matrix_include_entry_is_reported(self) -> None:
        # Red if the matrix-key reader misses `include:`, which would make the test above vacuous.
        workflow = WORKFLOW.read_text(encoding="utf-8")
        extended = workflow.replace(
            "        os: [ubuntu-24.04, macos-15]\n",
            "        os: [ubuntu-24.04, macos-15]\n        include:\n          - os: macos-14\n",
        )
        self.assertNotEqual(extended, workflow)
        self.assertEqual(unread_matrix_keys(extended), ["rust:include"])

    def test_a_changed_runner_image_changes_the_reported_names(self) -> None:
        # Red if the name reader ignores the matrix, which would make the test above vacuous.
        workflow = WORKFLOW.read_text(encoding="utf-8")
        bumped = workflow.replace("os: [ubuntu-24.04, macos-15]", "os: [ubuntu-24.04, macos-16]")
        self.assertNotEqual(bumped, workflow)
        self.assertIn("Rust (macos-16)", reported_check_names(bumped))
        self.assertNotIn("Rust (macos-15)", reported_check_names(bumped))

    def test_every_step_of_a_matrix_named_job_reads_the_change_scope(self) -> None:
        # Red if a step is added without the docs gate: on a documentation-only pull request it
        # would run with no checkout and turn a required check red.
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertEqual(ungated_steps(workflow), [])

    def test_a_step_without_the_gate_is_reported(self) -> None:
        # Red if the step reader misses an ungated step, which would make the test above vacuous.
        workflow = WORKFLOW.read_text(encoding="utf-8")
        gate = "      - if: ${{ needs.scope.outputs.scope != 'docs' }}\n        run: cargo fmt"
        stripped = workflow.replace(gate, "      - run: cargo fmt")
        self.assertNotEqual(stripped, workflow)
        self.assertEqual(len(ungated_steps(stripped)), 1)


if __name__ == "__main__":
    unittest.main()
