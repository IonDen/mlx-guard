"""Tests for the pure helpers of the tutorial example script.

The script lives under ``examples/tutorial`` and is not a package; it is loaded by path so the
suite has no dependency on ``mlx_lm`` (the script imports it lazily inside ``main``).
"""

import importlib.util
import json
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
        # Bug this catches: a page boundary that drops or duplicates text.
        text = "\n\n".join(f"paragraph {i} " + "x" * 50 for i in range(40))
        pages = _load().paginate(text, max_chars=400)
        self.assertEqual("".join(pages), text)
        self.assertGreater(len(pages), 1)

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

    def test_max_chars_below_one_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            _load().paginate("abc", max_chars=0)


class ProgressTests(unittest.TestCase):
    def test_missing_progress_file_means_nothing_done(self) -> None:
        module = _load()
        done = module.load_progress(Path("/nonexistent/progress.json"))
        self.assertEqual(done, {})

    def test_progress_round_trips_and_keeps_page_order_by_index(self) -> None:
        # Bug this catches: keys serialized as strings and never mapped back to page numbers.
        module = _load()
        with __import__("tempfile").TemporaryDirectory() as tmp:
            path = Path(tmp) / "progress.json"
            module.save_progress(path, {3: "three", 1: "one"})
            self.assertEqual(module.load_progress(path), {1: "one", 3: "three"})
            raw = json.loads(path.read_text())
            self.assertEqual(sorted(raw["summaries"]), ["1", "3"])

    def test_remaining_pages_skips_what_is_done(self) -> None:
        module = _load()
        self.assertEqual(module.remaining_pages(5, {0: "a", 3: "b"}), [1, 2, 4])


if __name__ == "__main__":
    unittest.main()
