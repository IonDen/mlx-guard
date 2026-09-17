#!/usr/bin/env python3
"""Draw docs/images/limit-intervention.svg from the recorded tutorial limit run.

Every plotted value, threshold, time and caption comes from committed evidence: the report's
samples, thresholds, signal and outcome, the transcript's last printed MLX figure, and the
bundle's provenance. The wording of the annotations and where they sit are fixed for this
recording. Run it with no arguments to rewrite the figure, or with --check to fail when the
committed figure no longer matches (python/tests/test_limit_figure.py does the same).
"""

from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import math
import re
import statistics
import sys
from dataclasses import dataclass
from pathlib import Path
from xml.sax.saxutils import escape, quoteattr

ROOT = Path(__file__).resolve().parents[1]
BUNDLE = ROOT / "evidence" / "v0.2.0" / "tutorial"
FIGURE = ROOT / "docs" / "images" / "limit-intervention.svg"

GIB = 1024**3
SIGTERM = 15

WIDTH = 720
HEIGHT = 340
PLOT_LEFT = 64.0
PLOT_RIGHT = 548.0
PLOT_TOP = 76.0
PLOT_BOTTOM = 272.0
MARGIN_X = 558.0

FONT = '-apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif'
INK = "#0f172a"
MUTED = "#64748b"
CURVE = "#334155"
GRID = "#e2e8f0"
AXIS = "#cbd5e1"
LIMIT = "#dc2626"
LIMIT_TEXT = "#b91c1c"
BAND = "#fef3c7"
BAND_TEXT = "#92400e"
PRINTED = "#0f766e"

_COUNT_WORDS = {1: "one", 2: "two", 3: "three", 4: "four", 5: "five"}

_PRINTED = re.compile(r"mlx active (\d+\.\d+) GiB")


@dataclass(frozen=True)
class Run:
    """The facts of one recorded enforcing run that the figure shows."""

    samples: tuple[tuple[int, int], ...]
    warning_bytes: int
    limit_bytes: int
    emergency_bytes: int
    breach_samples: int
    term_ms: int
    end_ms: int
    last_printed_gib: str
    source: str
    host: str
    macos: str
    guard_version: str


@dataclass(frozen=True)
class Scale:
    """Map run time and bytes onto the plot area."""

    max_ms: int
    max_bytes: int

    @classmethod
    def for_run(cls, run: Run) -> Scale:
        """Round the axes up to the next 5 s and the next whole GiB above the KILL line."""
        return cls(
            max_ms=math.ceil(run.end_ms / 5000) * 5000,
            max_bytes=(run.emergency_bytes // GIB + 1) * GIB,
        )

    def x(self, ms: int) -> float:
        """Return the horizontal position of a time."""
        return PLOT_LEFT + (PLOT_RIGHT - PLOT_LEFT) * ms / self.max_ms

    def y(self, size: int) -> float:
        """Return the vertical position of a byte count; more bytes sit higher."""
        return PLOT_BOTTOM - (PLOT_BOTTOM - PLOT_TOP) * size / self.max_bytes


def last_printed_gib(transcript: str) -> str:
    """Return the last MLX active-memory figure the job printed, as it printed it."""
    found = _PRINTED.findall(transcript)
    if not found:
        raise ValueError("the transcript has no 'mlx active' line")
    return str(found[-1])


def evidence_fingerprint(run: Run) -> str:
    """Return a short digest of every fact the figure uses.

    Coordinates are rounded to a tenth of a pixel, so a small change in the evidence can leave
    them untouched. The digest goes into the SVG so that any change to the run shows up there.
    """
    return hashlib.sha256(repr(run).encode("utf-8")).hexdigest()[:16]


def gib_label(size: int) -> str:
    """Format bytes as GiB with at most two decimals and no trailing zeros."""
    text = f"{size / GIB:.2f}".rstrip("0").rstrip(".")
    return f"{text} GiB"


def load_run(*, report: Path, transcript: Path, provenance: Path) -> Run:
    """Read the run from the committed report, transcript and provenance files."""
    data = json.loads(report.read_text(encoding="utf-8"))
    origin = json.loads(provenance.read_text(encoding="utf-8"))
    configuration = data["configuration"]
    samples = tuple(
        (int(sample["captured_at_ms"]), int(sample["aggregate_footprint_bytes"]["value"]))
        for sample in data["samples"]
        if sample["aggregate_footprint_bytes"]["status"] == "available"
    )
    term = next(signal for signal in data["signals"] if signal["signal"] == SIGTERM)
    memory_gb = int(origin["memory_bytes"]) // GIB
    return Run(
        samples=samples,
        warning_bytes=int(configuration["warning_footprint_bytes"]),
        limit_bytes=int(configuration["max_footprint_bytes"]),
        emergency_bytes=int(configuration["emergency_footprint_bytes"]),
        breach_samples=int(configuration["required_breach_samples"]),
        term_ms=int(term["at_ms"]),
        end_ms=int(data["outcome"]["at_ms"]),
        last_printed_gib=last_printed_gib(transcript.read_text(encoding="utf-8")),
        source=f"{report.parent.parent.relative_to(ROOT).as_posix()} ({report.name})",
        host=f"one {str(origin['chip']).removeprefix('Apple ')} {memory_gb} GB",
        macos=str(origin["macos"]["ProductVersion"]),
        guard_version=str(data["package_version"]),
    )


def _text(
    x: float,
    y: float,
    body: str,
    *,
    size: float,
    fill: str = INK,
    weight: int = 400,
    anchor: str = "start",
    identifier: str | None = None,
    halo: bool = False,
) -> str:
    """Return one <text> element; a halo paints a white edge so grid lines do not cross it."""
    name = f" id={quoteattr(identifier)}" if identifier else ""
    edge = ' stroke="#ffffff" stroke-width="4" paint-order="stroke"' if halo else ""
    return (
        f'  <text{name} x="{x:.1f}" y="{y:.1f}" font-size="{size:g}" font-weight="{weight}" '
        f'text-anchor="{anchor}" fill="{fill}"{edge}>{escape(body)}</text>'
    )


def render(run: Run) -> str:
    """Return the figure as SVG text; the same run always gives the same bytes."""
    scale = Scale.for_run(run)
    limit = gib_label(run.limit_bytes)
    warning = gib_label(run.warning_bytes)
    emergency = gib_label(run.emergency_bytes)
    term_s = f"{run.term_ms / 1000:.2f}"
    gone_s = f"{(run.end_ms - run.term_ms) / 1000:.2f}"
    count = len(run.samples)
    times = [at for at, _ in run.samples]
    gaps = [later - earlier for earlier, later in itertools.pairwise(times)]
    gap_ms = round(statistics.median(gaps))
    breaches = _COUNT_WORDS.get(run.breach_samples, str(run.breach_samples))
    term_bytes = next(size for at, size in run.samples if at == run.term_ms)
    y_limit, y_warning = scale.y(run.limit_bytes), scale.y(run.warning_bytes)
    y_emergency = scale.y(run.emergency_bytes)
    y_printed = scale.y(round(float(run.last_printed_gib) * GIB))
    x_term, y_term = scale.x(run.term_ms), scale.y(term_bytes)

    label = (
        f"Memory footprint of a leaking job over {run.end_ms / 1000:.0f} seconds, {count} samples. "
        f"The footprint rises and falls page by page, trending up into the warning band at "
        f"{warning}. After {breaches} samples in a row at or above the {limit} limit the "
        f"supervisor sends SIGTERM at {term_s} seconds and the process group is gone within "
        f"{gone_s} seconds. A sample at or above {emergency} would have meant KILL at once. "
        f"The last MLX active-memory figure the job itself printed was {run.last_printed_gib} GiB."
    )
    points = " ".join(f"{scale.x(at):.1f},{scale.y(size):.1f}" for at, size in run.samples)

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {HEIGHT}" '
        f'width="{WIDTH}" height="{HEIGHT}" role="img" aria-label={quoteattr(label)} '
        f'data-evidence="{evidence_fingerprint(run)}" '
        f"font-family={quoteattr(FONT)}>",
        f'  <rect x="0" y="0" width="{WIDTH}" height="{HEIGHT}" fill="#ffffff" rx="6"/>',
        _text(24, 30, f"A leaking job, stopped at its {limit} limit", size=16, weight=600),
        _text(
            24,
            50,
            f"Memory footprint macOS charged to the job's process group: {count} samples, "
            f"about {gap_ms} ms apart",
            size=12.5,
            fill="#475569",
        ),
        _text(PLOT_LEFT - 8, PLOT_TOP - 10, "GiB", size=12, fill=MUTED, anchor="end"),
    ]

    for step in range(0, scale.max_bytes // GIB + 1, 2):
        y = scale.y(step * GIB)
        if step:
            parts.append(
                f'  <line x1="{PLOT_LEFT:.1f}" y1="{y:.1f}" x2="{PLOT_RIGHT:.1f}" y2="{y:.1f}" '
                f'stroke="{GRID}" stroke-width="1"/>'
            )
        parts.append(_text(PLOT_LEFT - 8, y + 4, str(step), size=12, fill=MUTED, anchor="end"))

    parts += [
        f'  <rect id="warning-band" x="{PLOT_LEFT:.1f}" y="{y_limit:.1f}" '
        f'width="{PLOT_RIGHT - PLOT_LEFT:.1f}" height="{y_warning - y_limit:.1f}" fill="{BAND}"/>',
        _text(
            PLOT_LEFT + 8,
            (y_limit + y_warning) / 2 + 4,
            f"warning band, from {warning} (recorded, no action)",
            size=12,
            fill=BAND_TEXT,
        ),
        f'  <line id="emergency-line" x1="{PLOT_LEFT:.1f}" y1="{y_emergency:.1f}" '
        f'x2="{PLOT_RIGHT:.1f}" y2="{y_emergency:.1f}" stroke="{LIMIT_TEXT}" stroke-width="1.5" '
        'stroke-dasharray="5 4"/>',
        _text(MARGIN_X, y_emergency - 10, f"at {emergency} or above:", size=12, fill=LIMIT_TEXT),
        _text(MARGIN_X, y_emergency + 4, "KILL at once", size=12, fill=LIMIT_TEXT),
        f'  <line id="limit-line" x1="{PLOT_LEFT:.1f}" y1="{y_limit:.1f}" '
        f'x2="{PLOT_RIGHT:.1f}" y2="{y_limit:.1f}" stroke="{LIMIT}" stroke-width="1.5"/>',
        _text(
            MARGIN_X,
            y_limit + 4,
            f"your limit: {limit}",
            size=12,
            fill=LIMIT_TEXT,
            weight=600,
            identifier="limit-label",
        ),
        f'  <line x1="{PLOT_LEFT:.1f}" y1="{PLOT_BOTTOM:.1f}" x2="{PLOT_RIGHT:.1f}" '
        f'y2="{PLOT_BOTTOM:.1f}" stroke="{AXIS}" stroke-width="1"/>',
    ]

    for second in range(0, scale.max_ms // 1000 + 1, 10):
        body = "0" if second == 0 else f"{second} s"
        x = scale.x(second * 1000)
        parts.append(_text(x, PLOT_BOTTOM + 16, body, size=12, fill=MUTED, anchor="middle"))

    parts += [
        f'  <polyline id="footprint" points="{points}" fill="none" stroke="{CURVE}" '
        'stroke-width="1.5" stroke-linejoin="round"/>',
        _text(PLOT_LEFT + 22, scale.y(GIB) + 4, "the model loads", size=12, fill=MUTED),
        f'  <line x1="{x_term:.1f}" y1="{y_term + 6:.1f}" x2="{x_term:.1f}" y2="186.0" '
        f'stroke="{LIMIT}" stroke-width="1"/>',
        f'  <circle id="term-marker" cx="{x_term:.1f}" cy="{y_term:.1f}" r="4" fill="{LIMIT}" '
        'stroke="#ffffff" stroke-width="1.5"/>',
        _text(
            PLOT_RIGHT - 4,
            200,
            f"{breaches} samples in a row at or above the limit:",
            size=12,
            anchor="end",
            halo=True,
        ),
        _text(
            PLOT_RIGHT - 4,
            216,
            f"SIGTERM at {term_s} s, group gone within {gone_s} s",
            size=12,
            weight=600,
            anchor="end",
            halo=True,
        ),
        f'  <line x1="{PLOT_RIGHT:.1f}" y1="{y_printed:.1f}" x2="{PLOT_RIGHT + 7:.1f}" '
        f'y2="{y_printed:.1f}" stroke="{PRINTED}" stroke-width="2"/>',
        _text(
            MARGIN_X,
            y_printed + 4,
            f"MLX active {run.last_printed_gib} GiB:",
            size=12,
            fill=PRINTED,
        ),
        _text(MARGIN_X, y_printed + 19, "the job's last line", size=12, fill=PRINTED),
        _text(
            24,
            HEIGHT - 18,
            f"{run.source}, {run.host}, macOS {run.macos}, mlx-guard {run.guard_version}",
            size=12,
            fill=MUTED,
        ),
        "</svg>",
    ]
    return "\n".join(parts) + "\n"


def main(argv: list[str]) -> int:
    """Write the figure, or with --check compare it with the committed file."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if the committed figure differs")
    arguments = parser.parse_args(argv)
    svg = render(
        load_run(
            report=BUNDLE / "reports" / "run-limit.json",
            transcript=BUNDLE / "transcripts" / "run-limit.txt",
            provenance=BUNDLE / "provenance.json",
        )
    )
    if arguments.check:
        if not FIGURE.exists() or FIGURE.read_text(encoding="utf-8") != svg:
            stale = f"{FIGURE.relative_to(ROOT)} is stale; run scripts/render_limit_figure.py"
            print(stale, file=sys.stderr)
            return 1
        return 0
    FIGURE.parent.mkdir(parents=True, exist_ok=True)
    FIGURE.write_text(svg, encoding="utf-8")
    print(f"wrote {FIGURE.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
