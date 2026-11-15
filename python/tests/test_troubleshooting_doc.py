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
WARNINGS_JSON = FIXTURES / "warnings.json"
# The band lines' anchor (mlx_guard_core::warnings::WARNING_ANCHOR), a literal from the line text.
WARNING_ANCHOR = "warnings-during-a-run"
# The automatic-limit launch line's anchor (mlx_guard_core::warnings::AUTO_LIMIT_ANCHOR).
AUTO_LIMIT_ANCHOR = "the-automatic-limit"
# A GiB figure such as "6.00 GiB" or "24GiB". The automatic limit's sections show none: the number
# depends on the Mac, so the page shows the form of the line with placeholders.
GIB_FIGURE = re.compile(r"[0-9]+(\.[0-9]+)? ?GiB")
# The one size those sections may name: the fixed minimum below which `auto` refuses. It is the
# same on every Mac, so it is not a figure a reader could take for their own limit or reading.
# The lookbehind keeps "21 GiB minimum" and "0.1 GiB minimum" from counting as it.
FIXED_MINIMUM = re.compile(r"(?<![0-9.])1 GiB minimum")
# The two refusals of `--max-footprint auto` before launch (exit 64), as early.json names them.
AUTO_REFUSAL_CAUSES = ("auto_limit_unavailable", "auto_limit_below_floor")
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


def warning_lines(texts: list[str]) -> list[str]:
    """Return the band lines from ``warnings.json`` texts, each without its link."""
    return [text.split(MORE, 1)[0] for text in texts]


def corpus_lines() -> list[str]:
    """Return every sentence the binary prints, from all three corpora."""
    texts = [case["text"] for case in json.loads(CASES_JSON.read_text(encoding="utf-8"))]
    texts += [entry["text"] for entry in json.loads(EARLY_JSON.read_text(encoding="utf-8"))]
    warnings = [row["text"] for row in json.loads(WARNINGS_JSON.read_text(encoding="utf-8"))]
    return printed_lines(texts) + warning_lines(warnings)


def quote_pattern(quote: str) -> re.Pattern[str]:
    """Compile a page quote; a ``{placeholder}`` stands for any filled-in run value.

    The quote must start and end on a word boundary of the printed line, so a quote cut off
    inside a word does not match.
    """
    body = quote.lstrip(",; ")
    escaped = re.escape(body)
    escaped = re.sub(r"\\\{[a-z_]+\\\}", ".+?", escaped)
    return re.compile(r"(?:^|(?<=[\s(;,]))" + escaped + r"(?=$|[\s.,;:)])")


def quote_is_printed(quote: str, lines: list[str]) -> bool:
    """Whether the quote appears, word for word, inside one printed line."""
    pattern = quote_pattern(quote)
    return any(pattern.search(line) for line in lines)


def section(text: str, heading: str) -> str:
    """Return the body of the ``## {heading}`` section, up to the next level-2 heading."""
    marker = f"\n## {heading}\n"
    if marker not in text:
        raise AssertionError(f"no section headed {heading!r}")
    return text.split(marker, 1)[1].split("\n## ", 1)[0]


def gib_figures(text: str) -> list[str]:
    """Return every GiB figure in the text, as written."""
    return [match.group(0) for match in GIB_FIGURE.finditer(text)]


def limit_figures(text: str) -> list[str]:
    """Return the GiB figures that read as a limit or a reading: all but the fixed minimum.

    Whitespace is collapsed first, as Markdown renders it, so a phrase or a figure wrapped
    across a line break reads the same as on one line.
    """
    return gib_figures(FIXED_MINIMUM.sub("", re.sub(r"\s+", " ", text)))


def early_lines(cause: str) -> list[str]:
    """Return the two printed sentences of one ``early.json`` cause."""
    entries = json.loads(EARLY_JSON.read_text(encoding="utf-8"))
    texts = [entry["text"] for entry in entries if entry["cause"] == cause]
    if len(texts) != 1:
        raise AssertionError(f"early.json holds {len(texts)} rows for {cause!r}")
    return printed_lines(texts)


def quotes_printed_with(section_text: str, words: str, lines: list[str]) -> list[str]:
    """Return the section's quotes that contain ``words`` and are printed in ``lines``."""
    return [
        quote
        for quote in quoted_spans(section_text)
        if words in quote and quote_is_printed(quote, lines)
    ]


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

    def test_a_quote_cut_off_inside_a_word_does_not_match(self) -> None:
        # Bug: a pattern with no end boundary, so a quote that stops part-way through a printed
        # word ("launch" for "launched") still counts as printed.
        lines = list(self.lines)
        self.assertFalse(quote_is_printed("(--max-footprint), so nothing was launch", lines))
        self.assertFalse(quote_is_printed("run it again; if it repeats, open an iss", lines))
        self.assertTrue(quote_is_printed("(--max-footprint), so nothing was launched", lines))
        self.assertTrue(quote_is_printed("the command line was not accepted ({flag})", lines))
        self.assertTrue(quote_is_printed("if it repeats, open an issue.", lines))

    def test_printed_lines_drop_the_prefixes_and_the_link(self) -> None:
        # Bug: the link or the "what to do: " prefix left in, so no page quote of a to-do matches.
        text = "what happened: a b.\nwhat to do: c d. More: https://x/y.md#z"
        self.assertEqual(printed_lines([text]), ["a b.", "c d."])

    def test_warning_lines_drop_only_the_link(self) -> None:
        # Bug: a band line run through printed_lines (it has no "what to do" half), or its link
        # left in, so no page quote of its last clause matches.
        texts = ["warning: a b, at or above c. More: https://x/y.md#z", "warning cleared: d e."]
        self.assertEqual(
            warning_lines(texts), ["warning: a b, at or above c.", "warning cleared: d e."]
        )

    def test_section_stops_at_the_next_level_two_heading(self) -> None:
        # Bug: a section helper that runs on into the next section (its figures then count
        # against the wrong section) or that matches a heading by prefix ("## The automatic
        # limit, again"), or one that returns "" for a missing heading instead of failing.
        page = (
            "intro\n## The automatic limit\nbody one\n### sub\nstill one\n"
            "## The automatic limit, again\nbody two\n"
        )
        self.assertEqual(section(page, "The automatic limit"), "body one\n### sub\nstill one")
        with self.assertRaises(AssertionError):
            section(page, "64: the automatic limit")

    def test_gib_figures_find_every_written_form(self) -> None:
        # Bug: a figure pattern that misses the spaceless form (24GiB) or the whole-number form
        # (6 GiB), so a filled-in example passes the "no figure" check.
        self.assertEqual(
            gib_figures("a 6.00 GiB limit, 24GiB, 6 GiB, {limit} and GiB alone"),
            ["6.00 GiB", "24GiB", "6 GiB"],
        )
        self.assertEqual(
            gib_figures("`automatic limit: {limit}, {percent}% of the {estimate}`"), []
        )

    def test_limit_figures_let_through_only_the_fixed_minimum(self) -> None:
        # Bug: an exemption wider than the one fixed phrase, so a figure that reads as one Mac's
        # limit or reading ("auto picked 21.37 GiB", a bare "1 GiB") passes the no-figure check.
        self.assertEqual(limit_figures("auto refuses under the 1 GiB minimum."), [])
        self.assertEqual(limit_figures("auto picked 21.37 GiB for this run."), ["21.37 GiB"])
        self.assertEqual(
            limit_figures("auto with the 1 GiB minimum: under 1 GiB, nothing runs."), ["1 GiB"]
        )

    def test_limit_figures_do_not_exempt_a_figure_that_ends_in_the_phrase(self) -> None:
        # Bug: the phrase matched as a bare substring, so "21 GiB minimum" or "0.1 GiB minimum"
        # (a different number that only ends in "1") is taken for the fixed minimum.
        self.assertEqual(limit_figures("an auto 21 GiB minimum"), ["21 GiB"])
        self.assertEqual(limit_figures("an auto 0.1 GiB minimum"), ["0.1 GiB"])

    def test_limit_figures_read_a_phrase_or_figure_wrapped_across_lines(self) -> None:
        # Bug: the check reads the raw page lines, so the fixed phrase wrapped after "GiB" trips
        # it, and a figure wrapped between number and unit ("21.37\nGiB") slips through.
        self.assertEqual(limit_figures("under the 1 GiB\nminimum, so"), [])
        self.assertEqual(limit_figures("auto picked 21.37\nGiB here"), ["21.37 GiB"])

    def test_quotes_printed_with_needs_both_the_words_and_the_line(self) -> None:
        # Bug: a filter that keeps a quote with the words but not printed (a paraphrase), or a
        # printed quote without the words (the 0.5.0 advice kept in place of the new one).
        lines = ["fix --max-footprint as the error above says: give a size, or auto, then go."]
        page = (
            "`give a size, or auto, then go.` and `give a size, or auto, then stop.` "
            "and `fix --max-footprint as the error above says:`"
        )
        self.assertEqual(
            quotes_printed_with(page, "auto", lines), ["give a size, or auto, then go."]
        )

    def test_band_lines_count_as_printed(self) -> None:
        # Bug: warnings.json left out of the scan, so a page quote of a band line can never match.
        lines = corpus_lines()
        self.assertTrue(
            quote_is_printed(
                "at or below the {value} recovery threshold for your {limit} limit.", lines
            )
        )
        self.assertTrue(
            quote_is_printed(
                "and has not fallen to the {value} recovery threshold since {t};", lines
            )
        )


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

    def test_warning_anchor_has_a_heading(self) -> None:
        # Bug this catches: a launch or band line's "More:" link pointing at a section the page
        # does not have, the automatic-limit line linking the band lines' section (or a band line
        # linking the automatic limit's), or a corpus line linking any third anchor.
        rows = json.loads(WARNINGS_JSON.read_text(encoding="utf-8"))
        emitted: set[str] = set()
        for row in rows:
            linked = set(re.findall(r"TROUBLESHOOTING\.md#([^\s)]+)", row["text"]))
            emitted |= linked
            if row["kind"] == "auto":
                self.assertEqual(linked, {AUTO_LIMIT_ANCHOR}, f"row {row['row']}")
            elif linked:
                self.assertEqual(linked, {WARNING_ANCHOR}, f"row {row['row']}")
        self.assertEqual(
            emitted, {WARNING_ANCHOR, AUTO_LIMIT_ANCHOR}, "anchors linked from warnings.json"
        )
        slugs = heading_slugs(self.page)
        self.assertIn(WARNING_ANCHOR, slugs)
        self.assertIn(AUTO_LIMIT_ANCHOR, slugs)

    def test_automatic_limit_section_quotes_the_launch_line_as_a_form(self) -> None:
        # Bug this catches: "The automatic limit" quotes no launch line, quotes it with one Mac's
        # figures filled in instead of the placeholders, or quotes words the binary does not print.
        body = section(self.page, "The automatic limit")
        rows = json.loads(WARNINGS_JSON.read_text(encoding="utf-8"))
        launch = warning_lines([row["text"] for row in rows if row["kind"] == "auto"])
        self.assertTrue(launch, "warnings.json has no auto row")
        quotes = quotes_printed_with(body, "automatic limit: {limit}", launch)
        self.assertTrue(quotes, f"no launch-line quote; scanned: {quoted_spans(body)}")
        for quote in quotes:
            self.assertIn("{percent}%", quote)
            self.assertIn("{estimate}", quote)

    def test_automatic_limit_refusal_section_quotes_both_causes(self) -> None:
        # Bug this catches: "64: the automatic limit" quotes the refusal of only one cause (no
        # number, or a number under the 1 GiB minimum), or a reworded line for either, so a
        # reader who searches the page for the line on their screen does not find it.
        body = section(self.page, "64: the automatic limit")
        quotes = quoted_spans(body)
        missing = []
        for cause in AUTO_REFUSAL_CAUSES:
            happened, to_do = early_lines(cause)
            if not any(quote_is_printed(q, [happened]) and "auto" in q for q in quotes):
                missing.append(f"{cause}: {happened!r}")
            if not any(quote_is_printed(q, [to_do]) and "pass a size" in q for q in quotes):
                missing.append(f"{cause}: {to_do!r}")
        self.assertEqual(missing, [], f"lines the section does not quote; scanned: {quotes}")

    def test_automatic_limit_sections_show_no_figure(self) -> None:
        # Bug this catches: an example filled in with one Mac's numbers ("automatic limit:
        # 21.20 GiB"), which a reader takes as the number their Mac will get. Only the fixed
        # phrase "1 GiB minimum" may name a size.
        shown = {
            heading: limit_figures(section(self.page, heading))
            for heading in ("The automatic limit", "64: the automatic limit")
        }
        self.assertEqual(
            shown, {"The automatic limit": [], "64: the automatic limit": []}, "figures shown"
        )

    def test_command_line_section_quotes_the_auto_offer(self) -> None:
        # Bug this catches: "64: the command line" keeps only the 0.5.0 advice, which the other
        # flags still print, so the page never shows that a missing --max-footprint offers auto.
        body = section(self.page, "64: the command line")
        _, to_do = early_lines("command_line --max-footprint")
        self.assertTrue(
            quotes_printed_with(body, "auto", [to_do]),
            f"no quote of {to_do!r}; scanned: {quoted_spans(body)}",
        )

    def test_stop_sections_quote_the_automatic_forms(self) -> None:
        # Bug this catches: "75: the footprint limit" or "75: emergency kill" quotes only the
        # typed-size wording, so a reader of an automatic run's lines finds neither its "what
        # happened" nor its advice to pass a size on the page.
        lines = corpus_lines()
        for heading, advice in (
            ("75: the footprint limit", "headroom above that peak"),
            ("75: emergency kill", "pass a size to --max-footprint"),
        ):
            with self.subTest(section=heading):
                body = section(self.page, heading)
                self.assertTrue(
                    quotes_printed_with(body, "automatic limit mlx-guard chose at launch", lines),
                    f"no automatic 'what happened' quote; scanned: {quoted_spans(body)}",
                )
                self.assertTrue(
                    quotes_printed_with(body, advice, lines),
                    f"no automatic 'what to do' quote; scanned: {quoted_spans(body)}",
                )

    def test_warning_section_quotes_each_band_line(self) -> None:
        # Bug this catches: a quote in "Warnings during a run" that falls out of the quote scan
        # (a blank line or a stray backtick inside it), so the drift test below never checks it
        # and still passes; or a band line the section no longer quotes at all.
        section = self.page.split("\n## Warnings during a run\n", 1)[1].split("\n## ", 1)[0]
        quotes = quoted_spans(section)
        rows = json.loads(WARNINGS_JSON.read_text(encoding="utf-8"))
        missing = [
            kind
            for kind in ("entered", "still", "cleared")
            if not any(
                quote_is_printed(
                    quote, warning_lines([row["text"] for row in rows if row["kind"] == kind])
                )
                for quote in quotes
            )
        ]
        self.assertEqual(missing, [], f"band lines the section does not quote; scanned: {quotes}")

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
        # The band lines in warnings.json count as printed too.
        lines = corpus_lines()
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
