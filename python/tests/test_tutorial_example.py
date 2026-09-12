"""Tests for the pure helpers of the tutorial example script.

The script lives under ``examples/tutorial`` and is not a package; it is loaded by path so the
suite has no dependency on ``mlx_lm`` (the script imports it lazily inside ``main``).
"""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from types import ModuleType

SCRIPT = Path(__file__).resolve().parents[2] / "examples" / "tutorial" / "summarize_docs.py"


def _load() -> ModuleType:
    spec = importlib.util.spec_from_file_location("summarize_docs", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PaginateTests(unittest.TestCase):
    def test_every_character_lands_in_exactly_one_page_in_order(self) -> None:
        # Bug this catches: a page boundary that drops or duplicates text, or a page over the limit.
        text = "\n\n".join(f"paragraph {i} " + "x" * 50 for i in range(40))
        pages = _load().paginate(text, max_chars=400)
        self.assertEqual("".join(pages), text)
        self.assertGreater(len(pages), 1)
        self.assertTrue(all(len(page) <= 400 for page in pages))

    def test_a_page_may_be_exactly_max_chars_long(self) -> None:
        # Bug this catches: `>=` instead of `>` when deciding whether a paragraph still fits.
        text = "a" * 10 + "\n\n" + "b" * 8
        self.assertEqual(_load().paginate(text, max_chars=20), [text])

    def test_pages_break_on_paragraph_boundaries_when_possible(self) -> None:
        # Bug this catches: cutting mid-word when a blank line was available.
        text = "first paragraph\n\nsecond paragraph\n\nthird paragraph"
        pages = _load().paginate(text, max_chars=20)
        self.assertEqual(pages, ["first paragraph\n\n", "second paragraph\n\n", "third paragraph"])

    def test_a_paragraph_longer_than_a_page_is_split_hard(self) -> None:
        # Bug this catches: an infinite loop or an oversized page on a single long paragraph.
        text = "y" * 1000
        pages = _load().paginate(text, max_chars=300)
        self.assertEqual([len(p) for p in pages], [300, 300, 300, 100])

    def test_empty_text_has_no_pages(self) -> None:
        # Bug this catches: emitting one empty page, which would send an empty prompt to the model.
        self.assertEqual(_load().paginate("", max_chars=100), [])

    def test_max_chars_below_one_is_rejected(self) -> None:
        # Bug this catches: dropping the guard, which turns the hard split into an infinite loop.
        with self.assertRaises(ValueError):
            _load().paginate("abc", max_chars=0)


class ProgressTests(unittest.TestCase):
    def test_missing_progress_file_means_nothing_done(self) -> None:
        # Bug this catches: raising on a first run instead of starting from nothing.
        done = _load().load_progress(Path("/nonexistent/progress.json"), corpus_id="c1")
        self.assertEqual(done, {})

    def test_progress_round_trips_and_keeps_page_order_by_index(self) -> None:
        # Bug this catches: keys serialized as strings and never mapped back to page numbers, or
        # returned in file order instead of page order.
        module = _load()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "progress.json"
            module.save_progress(path, {3: "three", 1: "one"}, corpus_id="c1")
            loaded = module.load_progress(path, corpus_id="c1")
            self.assertEqual(loaded, {1: "one", 3: "three"})
            self.assertEqual(list(loaded), [1, 3])
            raw = json.loads(path.read_text())
            self.assertEqual(sorted(raw["summaries"]), ["1", "3"])

    def test_progress_from_another_corpus_is_refused(self) -> None:
        # Bug this catches: resuming with page indices that belong to different text.
        module = _load()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "progress.json"
            module.save_progress(path, {0: "zero"}, corpus_id="c1")
            with self.assertRaises(module.ProgressMismatch):
                module.load_progress(path, corpus_id="c2")

    def test_save_progress_returns_the_size_on_disk(self) -> None:
        # Bug this catches: reporting the in-memory string length, which differs for non-ASCII text.
        module = _load()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "progress.json"
            size = module.save_progress(path, {0: "résumé — ünïcode"}, corpus_id="c1")
            self.assertEqual(size, path.stat().st_size)

    def test_remaining_pages_skips_what_is_done(self) -> None:
        # Bug this catches: iterating the done keys instead of the page range, or an inverted test.
        module = _load()
        self.assertEqual(module.remaining_pages(5, {0: "a", 3: "b"}), [1, 2, 4])
        self.assertEqual(module.remaining_pages(3, {0: "a", 9: "stale"}), [1, 2])


class CollectDocumentsTests(unittest.TestCase):
    def test_files_are_concatenated_in_sorted_order_with_headings(self) -> None:
        # Bug this catches: relying on directory iteration order, which would make page N mean
        # different text on a resumed run.
        module = _load()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "b.md").write_text("bee")
            (root / "a.md").write_text("ay")
            (root / "sub").mkdir()
            (root / "sub" / "c.md").write_text("see")
            (root / "notes.txt").write_text("ignored")
            text = module.collect_documents([root])
            self.assertEqual(
                text,
                "# Document: a.md\n\nay\n\n# Document: b.md\n\nbee\n\n# Document: c.md\n\nsee\n\n",
            )

    def test_a_missing_path_is_an_error(self) -> None:
        # Bug this catches: a typo in --docs silently producing a zero-page run that exits 0.
        with self.assertRaises(FileNotFoundError):
            _load().collect_documents([Path("/nonexistent/docs")])

    def test_corpus_id_changes_with_text_and_page_size(self) -> None:
        # Bug this catches: a fingerprint that ignores one of the two inputs behind page N.
        module = _load()
        same = module.corpus_id("text", 100)
        self.assertEqual(same, module.corpus_id("text", 100))
        self.assertNotEqual(same, module.corpus_id("other", 100))
        self.assertNotEqual(same, module.corpus_id("text", 101))


if __name__ == "__main__":
    unittest.main()
