"""Limit derivation and band prediction from an observe run's own samples.

See review §"A method for L3 and F3".
"""

from collections.abc import Sequence
from itertools import pairwise
from typing import Literal


def late_peak(values: Sequence[int], fraction: float = 1 / 3) -> int:
    """Return the highest value over the last ``fraction`` of the series (the steady state).

    Excludes an early load spike, which would otherwise set the limit too high.
    """
    if not values:
        raise ValueError("no samples")
    tail = values[-max(1, int(len(values) * fraction)) :]
    return max(tail)


def derive_limit(p_late: int, ratio_percent: int = 105) -> int:
    """floor(P_late / 1.05): just under the steady peak, so the breach is a small step."""
    return p_late * 100 // ratio_percent


def emergency_threshold(limit: int) -> int:
    """Return the CLI's band rule: limit + max(1, floor(limit / 10)) (lib.rs:75-78)."""
    return limit + max(1, limit // 10)


def headroom_limit(peak_bytes: int, percent: int = 125) -> int:
    """Return a limit that must not fire: the repeatable peak plus 25% headroom, as whole bytes."""
    return peak_bytes * percent // 100


def largest_step_near(
    values: Sequence[int], limit: int, low_percent: int = 80, high_percent: int = 120
) -> int:
    """Return the largest consecutive-sample delta among pairs that touch [0.8·limit, 1.2·limit]."""
    low, high = limit * low_percent // 100, limit * high_percent // 100
    largest = 0
    for a, b in pairwise(values):
        if low <= a <= high or low <= b <= high:
            largest = max(largest, abs(b - a))
    return largest


def predict_band(values: Sequence[int], limit: int) -> Literal["graceful", "emergency"]:
    """Graceful if the footprint can dwell two samples inside a band 10 % wide; else emergency."""
    return "graceful" if largest_step_near(values, limit) < limit // 10 else "emergency"
