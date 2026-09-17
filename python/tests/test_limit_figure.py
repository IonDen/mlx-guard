"""Tests for the README figure drawn from the recorded limit run."""

from __future__ import annotations

import dataclasses
import importlib.util
import sys
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from types import ModuleType
from typing import Any

# Resolved from this file, not the CWD: the wheel proof runs the suite from a temp directory.
ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "render_limit_figure.py"
BUNDLE = ROOT / "evidence" / "v0.2.0" / "tutorial"
FIGURE = ROOT / "docs" / "images" / "limit-intervention.svg"
SVG = "{http://www.w3.org/2000/svg}"
GIB = 1024**3


def load_script() -> ModuleType:
    """Import the generator by path; `scripts/` is not a package."""
    spec = importlib.util.spec_from_file_location("render_limit_figure", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    # Dataclasses look their module up while the classes are being created, and only then.
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
    finally:
        del sys.modules[spec.name]
    return module


figure = load_script()


def recorded_run() -> Any:
    """Load the committed run the figure is drawn from."""
    return figure.load_run(
        report=BUNDLE / "reports" / "run-limit.json",
        transcript=BUNDLE / "transcripts" / "run-limit.txt",
        provenance=BUNDLE / "provenance.json",
    )


def element(svg: str, identifier: str) -> ET.Element:
    """Return the one element with this id."""
    found = [node for node in ET.fromstring(svg).iter() if node.get("id") == identifier]  # noqa: S314
    assert len(found) == 1, (identifier, len(found))
    return found[0]


class LimitFigureTests(unittest.TestCase):
    """Each test names the one-line defect that would turn it red."""

    def test_committed_figure_matches_the_generator(self) -> None:
        # Red if the evidence, the generator or the committed SVG changes without the other two.
        self.assertEqual(figure.render(recorded_run()), FIGURE.read_text(encoding="utf-8"))

    def test_run_facts_come_from_the_right_report_fields(self) -> None:
        # Red if the loader reads a neighbouring field: the recovery threshold for the warning
        # band, the outcome time for the signal time, or the first printed figure for the last.
        run = recorded_run()
        self.assertEqual(len(run.samples), 702)
        self.assertEqual(run.samples[-1], (39964, 6499453256))
        self.assertEqual(run.warning_bytes, 5798205850)
        self.assertEqual(run.limit_bytes, 6 * GIB)
        self.assertEqual(run.emergency_bytes, 7086696038)
        self.assertEqual(run.term_ms, 39964)
        self.assertEqual(run.end_ms, 40016)
        self.assertEqual(run.last_printed_gib, "5.07")

    def test_last_printed_figure_is_the_last_mlx_line_of_a_transcript(self) -> None:
        # Red if the parser takes the first match, or a number from another line.
        text = (
            "mlx-guard: enforcing a 6442450944-byte footprint limit\n"
            "page 1/3 in 1.2s; mlx active 1.79 GiB; caches held 1\n"
            "page 2/3 in 0.9s; mlx active 4.20 GiB; caches held 2\n"
            "mlx-guard: policy_intervention at 40016ms; 702 samples, 1 signal\n"
        )
        self.assertEqual(figure.last_printed_gib(text), "4.20")

    def test_byte_labels_drop_trailing_zeros(self) -> None:
        # Red if a label prints 6.00 GiB, or rounds 6.6 GiB to 7.
        self.assertEqual(figure.gib_label(6 * GIB), "6 GiB")
        self.assertEqual(figure.gib_label(7086696038), "6.6 GiB")
        self.assertEqual(figure.gib_label(5798205850), "5.4 GiB")

    def test_scale_maps_the_run_into_the_plot_area(self) -> None:
        # Red if an axis is flipped or the plot does not span the run.
        run = recorded_run()
        scale = figure.Scale.for_run(run)
        self.assertEqual(scale.x(0), figure.PLOT_LEFT)
        self.assertEqual(scale.x(scale.max_ms), figure.PLOT_RIGHT)
        self.assertEqual(scale.y(0), figure.PLOT_BOTTOM)
        self.assertEqual(scale.y(scale.max_bytes), figure.PLOT_TOP)
        self.assertGreaterEqual(scale.max_ms, run.end_ms)
        self.assertGreater(scale.max_bytes, run.emergency_bytes)
        self.assertLess(scale.y(run.limit_bytes), scale.y(run.warning_bytes))

    def test_threshold_lines_sit_at_the_scaled_report_values(self) -> None:
        # Red if a line is drawn at a typed-in position instead of the scaled report value.
        run = recorded_run()
        scale = figure.Scale.for_run(run)
        svg = figure.render(run)
        for identifier, value in (
            ("limit-line", run.limit_bytes),
            ("emergency-line", run.emergency_bytes),
        ):
            line = element(svg, identifier)
            self.assertEqual(line.get("y1"), f"{scale.y(value):.1f}")
            self.assertEqual(line.get("y2"), f"{scale.y(value):.1f}")
        band = element(svg, "warning-band")
        self.assertEqual(band.get("y"), f"{scale.y(run.limit_bytes):.1f}")
        height = scale.y(run.warning_bytes) - scale.y(run.limit_bytes)
        self.assertEqual(band.get("height"), f"{height:.1f}")

    def test_term_marker_sits_on_the_sample_that_tripped_the_limit(self) -> None:
        # Red if the marker uses the outcome time, or floats off the curve.
        run = recorded_run()
        scale = figure.Scale.for_run(run)
        marker = element(figure.render(run), "term-marker")
        self.assertEqual(marker.get("cx"), f"{scale.x(39964):.1f}")
        self.assertEqual(marker.get("cy"), f"{scale.y(6499453256):.1f}")

    def test_a_different_limit_moves_the_line_and_rewrites_its_label(self) -> None:
        # Red if the limit label or position is hardcoded for the recorded 6 GiB run.
        run = recorded_run()
        lower = dataclasses.replace(run, limit_bytes=5 * GIB)
        before = element(figure.render(run), "limit-line").get("y1")
        after_svg = figure.render(lower)
        self.assertNotEqual(element(after_svg, "limit-line").get("y1"), before)
        self.assertIn("5 GiB", "".join(element(after_svg, "limit-label").itertext()))
        self.assertNotIn("6 GiB", "".join(element(after_svg, "limit-label").itertext()))

    def test_a_one_byte_change_in_the_evidence_changes_the_figure(self) -> None:
        # Red if the figure only depends on rounded positions: a small edit to one sample moves
        # no coordinate by a tenth of a pixel, and the drift test would then miss it.
        run = recorded_run()
        at, size = run.samples[300]
        nudged = (*run.samples[:300], (at, size + 1), *run.samples[301:])
        changed = dataclasses.replace(run, samples=nudged)
        self.assertNotEqual(figure.render(changed), figure.render(run))

    def test_curve_has_one_point_per_sample(self) -> None:
        # Red if the curve is thinned, which would hide the per-page swings.
        points = element(figure.render(recorded_run()), "footprint").get("points", "").split()
        self.assertEqual(len(points), 702)

    def test_svg_is_accessible_and_self_contained(self) -> None:
        # Red if the label goes missing, or the file starts loading anything from elsewhere:
        # GitHub and PyPI show it through an <img>, where scripts and external references die.
        svg = figure.render(recorded_run())
        root = ET.fromstring(svg)  # noqa: S314
        self.assertEqual(root.get("role"), "img")
        label = root.get("aria-label", "")
        for fact in ("6 GiB", "SIGTERM", "702"):
            self.assertIn(fact, label)
        self.assertEqual(root.get("viewBox"), f"0 0 {figure.WIDTH} {figure.HEIGHT}")
        tags = {node.tag.removeprefix(SVG) for node in root.iter()}
        self.assertFalse(tags & {"script", "image", "use", "foreignObject", "a"})
        self.assertNotIn("href", svg)
        self.assertEqual(svg.count("http"), 1)  # the xmlns declaration only

    def test_every_label_stays_inside_the_card(self) -> None:
        # Red if a label runs off either side of the card, whatever its anchor. Text width cannot
        # be measured without the font, so a rough 0.6 em per character stands in for it.
        root = ET.fromstring(figure.render(recorded_run()))  # noqa: S314
        texts = [node for node in root.iter(f"{SVG}text")]
        self.assertGreater(len(texts), 8)
        for node in texts:
            x, y = float(node.get("x", "0")), float(node.get("y", "0"))
            self.assertTrue(0 <= x <= figure.WIDTH and 0 < y <= figure.HEIGHT, node.attrib)
            body = "".join(node.itertext())
            width = 0.6 * float(node.get("font-size", "12")) * len(body)
            anchor = node.get("text-anchor", "start")
            left = {"start": x, "middle": x - width / 2, "end": x - width}[anchor]
            self.assertGreaterEqual(left, -1, body)
            self.assertLessEqual(left + width, figure.WIDTH + 1, body)


if __name__ == "__main__":
    unittest.main()
