"""Tests that the CI workflow reports every required check name on a documentation-only change."""

from __future__ import annotations

import re
import unittest
from pathlib import Path

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml"

_JOB = re.compile(r"^  ([A-Za-z_][\w-]*):\s*$")
_JOB_KEY = re.compile(r"^    (name|if):\s*(.*)$")


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
            current[key.group(1)] = key.group(2)
    return jobs


class RequiredCheckNameTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red."""

    def test_parser_reads_job_level_keys_and_not_step_conditions(self) -> None:
        # Red if the parser mistakes a step-level `if:` (six spaces or more) for a job-level one,
        # which would make the real assertion below fail on a correctly gated workflow.
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

    def test_matrix_named_jobs_are_never_skipped_at_job_level_by_change_scope(self) -> None:
        # Red if a job whose check name comes from the matrix is gated on the change scope with a
        # job-level `if:`. GitHub does not expand the matrix of a skipped job, so it reports one
        # check under the literal template name and the required expanded names (for example
        # `Rust (macos-15)`) never report: a documentation-only pull request can then never merge.
        jobs = job_level_keys(WORKFLOW.read_text(encoding="utf-8"))
        matrix_named = {
            job: keys for job, keys in jobs.items() if "${{ matrix." in keys.get("name", "")
        }
        self.assertTrue(matrix_named, "expected at least one matrix-named job in ci.yml")
        offenders = sorted(
            job for job, keys in matrix_named.items() if "scope" in keys.get("if", "")
        )
        self.assertEqual(offenders, [])


if __name__ == "__main__":
    unittest.main()
