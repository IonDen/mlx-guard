"""Tests for the CI change-scope classifier that lets documentation-only PRs skip code jobs."""

from __future__ import annotations

import subprocess
import unittest
from pathlib import Path

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
SCRIPT = Path(__file__).resolve().parents[2] / "scripts/ci-change-scope.sh"


def classify(paths: list[str]) -> str:
    """Feed one changed path per line to the classifier and return its verdict."""
    completed = subprocess.run(
        [str(SCRIPT)],
        input="".join(f"{path}\n" for path in paths),
        capture_output=True,
        text=True,
        check=True,
    )
    return completed.stdout.strip()


class ChangeScopeTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red."""

    def test_documentation_only_change_set_is_docs(self) -> None:
        # Red if the allowlist drops any of these surfaces, or the script defaults to "code".
        changed = [
            "README.md",
            "CHANGELOG.md",
            "docs/CLI.md",
            "docs/integrations/WRAP_A_COMMAND.md",
            "docs/papers/diagrams/supervision-boundary.svg",
            "evidence/v0.2.0/m1-max-32gb/README.md",
        ]
        self.assertEqual(classify(changed), "docs")

    def test_one_code_path_makes_the_whole_set_code(self) -> None:
        # Red if the script decides by majority instead of any-code-wins.
        self.assertEqual(classify(["README.md", "crates/mlx-guard-core/src/policy.rs"]), "code")

    def test_workflow_and_packaging_changes_are_code(self) -> None:
        # Red if .github/ or pyproject.toml slip into the documentation allowlist.
        self.assertEqual(classify([".github/workflows/ci.yml"]), "code")
        self.assertEqual(classify(["pyproject.toml"]), "code")
        self.assertEqual(classify(["scripts/test-wheel.sh"]), "code")

    def test_markdown_outside_the_allowlist_is_code(self) -> None:
        # Red if the allowlist matches by extension or by an unanchored basename.
        self.assertEqual(classify(["crates/mlx-guard-core/README.md"]), "code")

    def test_last_path_without_trailing_newline_still_counts(self) -> None:
        # Red if the read loop drops an unterminated final line: the code path would vanish and
        # the set would be waved through as documentation, which is the fail-open direction.
        completed = subprocess.run(
            [str(SCRIPT)],
            input="README.md\ncrates/mlx-guard-core/src/policy.rs",
            capture_output=True,
            text=True,
            check=True,
        )
        self.assertEqual(completed.stdout.strip(), "code")

    def test_empty_change_set_fails_safe_to_code(self) -> None:
        # Red if an empty diff (unknown base, force-push) is read as "nothing to test".
        self.assertEqual(classify([]), "code")


if __name__ == "__main__":
    unittest.main()
