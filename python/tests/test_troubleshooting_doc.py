"""Tests that keep docs/TROUBLESHOOTING.md in step with the explanation lines mlx-guard prints.

The anchor list has one source: ``crates/mlx-guard-core/tests/fixtures/explain/anchors.txt``.
The Rust test ``anchors_match_python`` pins ``mlx_guard_core::explain::ANCHORS`` to that file,
and the tests of ``mlx_guard._explain`` pin ``_explain.ANCHORS`` to it as well. Checking the page
against the file here checks it against both languages transitively, so this module does not
import ``_explain`` (it is not in every tree this test runs in).
"""

import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PAGE = ROOT / "docs" / "TROUBLESHOOTING.md"
README = ROOT / "README.md"
FIXTURES = ROOT / "crates" / "mlx-guard-core" / "tests" / "fixtures" / "explain"
ANCHORS_TXT = FIXTURES / "anchors.txt"
CASES_JSON = FIXTURES / "cases.json"
EARLY_JSON = FIXTURES / "early.json"
# An inline-code span is taken as a quote of the binary when it has at least this many words.
# Shorter spans are flags, field names or values (`--max-footprint`, `outcome.kind`, `0700`).
QUOTE_MIN_WORDS = 5

# The exit-code contract (README "When a run stops", docs/CLI.md). Literals from the contract, not
# read from either document, so a row dropped from both still fails.
CONTRACT_EXIT_ROWS = frozenset(
    {"the command's own code", "64", "70", "74", "75", "126", "127", "128 + n"}
)
EXAMPLE_REPORT = "evidence/v0.2.0/tutorial/reports/run-limit.json"
LINE_PREFIX = "mlx-guard: "
MORE = " More: "


def github_slug(heading: str) -> str:
    """Return the GitHub anchor for a heading: lowercase, keep alphanumerics, spaces, hyphens."""
    kept = "".join(ch for ch in heading.lower() if ch.isalnum() or ch in " -")
    return kept.replace(" ", "-")


def _outside_fences(text: str) -> list[str]:
    lines: list[str] = []
    fenced = False
    for line in text.splitlines():
        if line.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if not fenced:
            lines.append(line)
    return lines


def heading_slugs(text: str) -> list[str]:
    """Return the slug of every ATX heading that is not inside a fenced block."""
    slugs = []
    for line in _outside_fences(text):
        match = re.match(r"#{1,6} +(.+?) *#* *$", line)
        if match:
            slugs.append(github_slug(match.group(1)))
    return slugs


def table_rows(text: str, header_first_cell: str) -> list[list[str]]:
    """Return the body rows of the first table whose header starts with ``header_first_cell``."""
    lines = _outside_fences(text)
    for index, line in enumerate(lines):
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if line.lstrip().startswith("|") and cells[0] == header_first_cell:
            rows = []
            for body in lines[index + 2 :]:
                if not body.lstrip().startswith("|"):
                    break
                rows.append([cell.strip() for cell in body.strip().strip("|").split("|")])
            return rows
    raise AssertionError(f"no table headed {header_first_cell!r}")


def exit_row_keys(first_cell: str) -> set[str]:
    """Split an exit-code cell into contract keys: ``126`, `127`` gives two, prose gives one."""
    plain = first_cell.replace("`", "").strip()
    if plain.lower().startswith("the command's own code"):
        return {"the command's own code"}
    return {part.strip() for part in plain.split(",")}


def quoted_spans(text: str) -> list[str]:
    """Return every inline-code span of at least ``QUOTE_MIN_WORDS`` words outside fences.

    A span may wrap across lines inside a paragraph; its whitespace is collapsed to single spaces,
    as Markdown renders it.
    """
    spans = []
    for paragraph in re.split(r"\n[ \t]*\n", "\n".join(_outside_fences(text))):
        flat = re.sub(r"\s+", " ", paragraph)
        for span in re.findall(r"`([^`]+)`", flat):
            words = span.strip()
            if len(words.split(" ")) >= QUOTE_MIN_WORDS:
                spans.append(words)
    return spans


def printed_lines(texts: list[str]) -> list[str]:
    """Split corpus texts into the sentences the binary prints, without prefixes or the link."""
    lines = []
    for text in texts:
        happened, to_do = text.split("\n")
        lines.append(happened.removeprefix("what happened: "))
        lines.append(to_do.removeprefix("what to do: ").split(MORE, 1)[0])
    return lines


def quote_pattern(quote: str) -> re.Pattern[str]:
    """Compile a page quote; a ``{placeholder}`` stands for any filled-in run value."""
    body = quote.lstrip(",; ")
    escaped = re.escape(body)
    escaped = re.sub(r"\\\{[a-z_]+\\\}", ".+?", escaped)
    return re.compile(r"(?:^|(?<=[\s(;,]))" + escaped)


def quote_is_printed(quote: str, lines: list[str]) -> bool:
    """Whether the quote appears, word for word, inside one printed line."""
    pattern = quote_pattern(quote)
    return any(pattern.search(line) for line in lines)


def example_lines(text: str) -> list[str]:
    """Return the lines of the first fenced block under the "A worked example" heading."""
    section = text.split("\n## A worked example\n", 1)[1].split("\n## ", 1)[0]
    block = section.split("```text\n", 1)[1].split("```", 1)[0]
    return block.rstrip("\n").split("\n")


class SlugRuleTests(unittest.TestCase):
    def test_slug_follows_the_github_rule(self) -> None:
        # Bug this catches: a slug helper that keeps the colon or drops the hyphen, so every
        # other test in this file compares against anchors GitHub never generates.
        self.assertEqual(github_slug("64: the command line"), "64-the-command-line")
        self.assertEqual(github_slug("75: the wall-time limit"), "75-the-wall-time-limit")
        self.assertEqual(
            github_slug("Signal passed on by mlx-guard"), "signal-passed-on-by-mlx-guard"
        )
        self.assertEqual(github_slug("`code` (A, b)"), "code-a-b")


class QuoteRuleTests(unittest.TestCase):
    """The helpers the page-quote test relies on, each pinned on a small literal case."""

    lines = (
        "the command line was not accepted (--max-footprint), so nothing was launched.",
        "run it again; if it repeats, open an issue.",
    )

    def test_spans_are_long_inline_code_joined_across_lines(self) -> None:
        # Bug: a wrapped quote split at its line break (never matched), a short flag taken as a
        # sentence, or a fenced example line scanned as prose.
        page = (
            "It prints: `the command line was not\naccepted, so nothing ran.` See `--report`.\n"
            "\n```text\n`inside a fence is never a quote at all`\n```\n"
        )
        self.assertEqual(quoted_spans(page), ["the command line was not accepted, so nothing ran."])

    def test_a_quote_matches_only_its_exact_words(self) -> None:
        # Bug: a matcher that ignores one changed word, or a placeholder that is not a wildcard.
        lines = list(self.lines)
        self.assertTrue(quote_is_printed("the command line was not accepted ({flag}), so", lines))
        self.assertTrue(quote_is_printed("; if it repeats, open an issue.", lines))
        self.assertFalse(quote_is_printed("the command line was not accepted, so nothing", lines))
        self.assertFalse(quote_is_printed("run it again; if it repeats, open a ticket.", lines))
        self.assertFalse(
            quote_is_printed("line was not accepted", ["the command outline was not accepted"])
        )

    def test_printed_lines_drop_the_prefixes_and_the_link(self) -> None:
        # Bug: the link or the "what to do: " prefix left in, so no page quote of a to-do matches.
        text = "what happened: a b.\nwhat to do: c d. More: https://x/y.md#z"
        self.assertEqual(printed_lines([text]), ["a b.", "c d."])


class TroubleshootingPageTests(unittest.TestCase):
    page: str

    @classmethod
    def setUpClass(cls) -> None:
        cls.page = PAGE.read_text(encoding="utf-8")

    def test_every_emitted_anchor_has_a_heading(self) -> None:
        # Bug this catches: a heading renamed or removed, so a "More:" link the binary prints
        # lands on the top of the page instead of its section.
        listed = {line.strip() for line in ANCHORS_TXT.read_text(encoding="utf-8").splitlines()}
        listed.discard("")
        cases = json.loads(CASES_JSON.read_text(encoding="utf-8"))
        emitted = {
            m.group(1)
            for case in cases
            for m in re.finditer(r"TROUBLESHOOTING\.md#([^\s)]+)", case["text"])
        }
        self.assertTrue(listed, "anchors.txt is empty")
        self.assertTrue(emitted, "no anchors found in cases.json")
        slugs = set(heading_slugs(self.page))
        self.assertEqual(sorted(listed - slugs), [], "anchors.txt entries with no heading")
        self.assertEqual(sorted(emitted - slugs), [], "cases.json anchors with no heading")

    def test_every_readme_exit_row_has_a_section(self) -> None:
        # Bug this catches: an exit code row in the README (or the page's own table) with no
        # section on the page, or a page table link that points at no heading.
        readme_keys: set[str] = set()
        for row in table_rows(README.read_text(encoding="utf-8"), "Exit code"):
            readme_keys |= exit_row_keys(row[0])
        self.assertEqual(readme_keys, CONTRACT_EXIT_ROWS, "README exit rows")

        slugs = set(heading_slugs(self.page))
        page_keys: set[str] = set()
        for row in table_rows(self.page, "Exit code"):
            keys = exit_row_keys(row[0])
            page_keys |= keys
            links = re.findall(r"\]\(#([^)]+)\)", row[1])
            self.assertTrue(links, f"page row {row[0]!r} links no section")
            self.assertEqual(
                sorted(set(links) - slugs), [], f"page row {row[0]!r} links missing sections"
            )
        self.assertEqual(page_keys, readme_keys, "page table rows differ from README rows")

        for code in sorted(readme_keys & {"64", "70", "74", "75", "126", "127"}):
            with self.subTest(code=code):
                self.assertTrue(
                    any(slug.startswith(f"{code}-") for slug in slugs),
                    f"no section heading for exit {code}",
                )

    def test_rendered_example_matches_the_corpus(self) -> None:
        # Bug this catches: the worked example drifting from the sentences the binary prints for
        # the recorded run-limit report (one changed word, a dropped clause, a reworded line).
        cases = json.loads(CASES_JSON.read_text(encoding="utf-8"))
        matches = [case for case in cases if case["report"] == EXAMPLE_REPORT]
        self.assertEqual(len(matches), 1, "cases.json run-limit entries")
        expected = matches[0]["text"].split(MORE, 1)[0]

        lines = example_lines(self.page)
        for line in lines:
            self.assertTrue(line.startswith(LINE_PREFIX), f"line without prefix: {line!r}")
        shown = "\n".join(line[len(LINE_PREFIX) :] for line in lines)
        self.assertEqual(shown, expected)

    def test_every_quoted_sentence_is_one_the_binary_prints(self) -> None:
        # Bug this catches: a page quote that drifts from the binary's own sentences (a reworded
        # 64, 70 or 74 line, an old fixture value, a clause the explainer no longer prints).
        # Both corpora are checked against the binary by the native and Python goldens.
        texts = [case["text"] for case in json.loads(CASES_JSON.read_text(encoding="utf-8"))]
        texts += [entry["text"] for entry in json.loads(EARLY_JSON.read_text(encoding="utf-8"))]
        lines = printed_lines(texts)
        quotes = quoted_spans(self.page)
        self.assertGreater(len(quotes), 40, "the quote scan found almost nothing on the page")
        unmatched = [quote for quote in quotes if not quote_is_printed(quote, lines)]
        self.assertFalse(
            unmatched,
            f"{len(unmatched)} of {len(quotes)} page quotes are not printed by the binary:\n"
            + "\n".join(unmatched),
        )


if __name__ == "__main__":
    unittest.main()
