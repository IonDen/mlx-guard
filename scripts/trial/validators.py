"""Pure report-shape assertions for the in-house trial. Reads plain ``json.load`` output only."""

from collections.abc import Mapping, Sequence
from typing import Any, Literal

Report = Mapping[str, Any]
Band = Literal["graceful", "emergency", "none"]
RING_CAPACITY = 4096
_SIGUSR1, _SIGTERM, _SIGKILL = 30, 15, 9


class ShapeError(AssertionError):
    """A report does not have the shape the trial expected."""


def available_values(report: Report) -> list[int]:
    """Footprint values of samples whose whole owned group was measured."""
    return [
        int(s["aggregate_footprint_bytes"]["value"])
        for s in report["samples"]
        if s["aggregate_footprint_bytes"]["status"] == "available"
    ]


def unavailable_count(report: Report) -> int:
    """Count samples the peak did not see."""
    return sum(
        1 for s in report["samples"] if s["aggregate_footprint_bytes"]["status"] != "available"
    )


def peak(report: Report) -> int:
    """Highest available aggregate footprint; ShapeError when nothing was measured."""
    values = available_values(report)
    if not values:
        raise ShapeError("no available footprint sample")
    return max(values)


def ring_wrapped(report: Report) -> bool:
    """Return True when the 4,096-sample FIFO is full, so the earliest samples were evicted."""
    return len(report["samples"]) >= RING_CAPACITY


def _outcome_is(report: Report, kind: str) -> None:
    if report["outcome"]["kind"] != kind:
        raise ShapeError(f"expected outcome {kind}, got {report['outcome']}")


def assert_child_exit(report: Report, code: int) -> None:
    """Assert the child exited on its own with ``code``, and the supervisor never signalled it."""
    _outcome_is(report, "child_exited")
    if report["outcome"].get("code") != code:
        raise ShapeError(f"expected exit code {code}, got {report['outcome']}")
    if report["signals"]:
        raise ShapeError(f"expected no signals, got {report['signals']}")
    if report["outcome"].get("owned_group_survivors"):
        raise ShapeError("owned-group survivors were reported")


def assert_uneventful(report: Report, *, code: int = 0) -> None:
    """Assert a clean supervised finish: the given exit code, no signals, no checkpoint attempt."""
    assert_child_exit(report, code)
    if report["checkpoint"] != {"status": "not_negotiated"}:
        raise ShapeError(f"expected a bare not_negotiated checkpoint, got {report['checkpoint']}")


def _first_signal(report: Report) -> Mapping[str, Any]:
    signals: Sequence[Mapping[str, Any]] = report["signals"]
    if not signals:
        raise ShapeError("intervention without a signal record")
    return signals[0]


def assert_graceful_footprint(report: Report) -> None:
    """Assert a graceful footprint breach.

    A checkpoint attempt toward no endpoint, then TERM (0063's box).
    """
    _outcome_is(report, "policy_intervention")
    if report["checkpoint"] != {"status": "not_negotiated", "reason": "footprint"}:
        raise ShapeError(
            f"expected not_negotiated with reason footprint, got {report['checkpoint']}"
        )
    first = _first_signal(report)
    if (first["signal"], first["target"], first.get("reason")) != (
        _SIGTERM,
        "owned_process_group",
        "footprint",
    ):
        raise ShapeError(f"expected a footprint TERM to the owned group first, got {first}")
    for extra in report["signals"][1:]:
        if (extra["signal"], extra.get("reason")) != (_SIGKILL, "footprint"):
            raise ShapeError(f"only a grace-expiry KILL may follow the TERM, got {extra}")
    if any(t["to"] == "emergency" for t in report["transitions"]):
        raise ShapeError("graceful arm entered the emergency band")


def assert_emergency_footprint(report: Report) -> None:
    """Assert a single-sample emergency.

    One footprint KILL; the checkpoint record stays bare (review C1).
    """
    _outcome_is(report, "policy_intervention")
    if report["checkpoint"] != {"status": "not_negotiated"}:
        raise ShapeError(
            f"emergency path must not latch a checkpoint reason, got {report['checkpoint']}"
        )
    signals = report["signals"]
    if len(signals) != 1:
        raise ShapeError(f"expected exactly one KILL, got {signals}")
    s = signals[0]
    if (s["signal"], s["target"], s["result"], s.get("reason")) != (
        _SIGKILL,
        "owned_process_group",
        "delivered",
        "footprint",
    ):
        raise ShapeError(f"expected a delivered footprint KILL to the owned group, got {s}")
    if not any(t["to"] == "emergency" for t in report["transitions"]):
        raise ShapeError("no transition into the emergency state")


def observed_band(report: Report) -> Band:
    """Which band an enforcing arm landed in, from its first signal."""
    if report["outcome"]["kind"] != "policy_intervention" or not report["signals"]:
        return "none"
    first = report["signals"][0]["signal"]
    return "graceful" if first == _SIGTERM else "emergency" if first == _SIGKILL else "none"


def assert_cooperative(report: Report, *, reason: str) -> Mapping[str, Any]:
    """Negotiated checkpoint acknowledged, then TERM: the shape the resume flow depends on."""
    _outcome_is(report, "policy_intervention")
    cp: Mapping[str, Any] = report["checkpoint"]
    if cp.get("status") != "acknowledged_unverified_durability":
        raise ShapeError(f"expected an acknowledged checkpoint, got {cp}")
    request_id = cp.get("request_id")
    if type(request_id) is not int or request_id == 0:
        raise ShapeError(f"request_id must be a nonzero integer, got {request_id!r}")
    if cp.get("reason") != reason:
        raise ShapeError(f"expected checkpoint reason {reason}, got {cp.get('reason')}")
    if cp.get("artifact", {}).get("kind") not in ("file", "directory", "opaque"):
        raise ShapeError(f"expected a path-free artifact kind, got {cp.get('artifact')}")
    heads = [(s["signal"], s["target"]) for s in report["signals"][:2]]
    if heads != [(_SIGUSR1, "cooperative_endpoint"), (_SIGTERM, "owned_process_group")]:
        raise ShapeError(
            f"expected SIGUSR1 to the endpoint then TERM to the group, got {report['signals']}"
        )
    if any(s.get("reason") != reason for s in report["signals"]):
        raise ShapeError(f"every signal must carry reason {reason}: {report['signals']}")
    return cp


def assert_tty_probe(probe: Mapping[str, Any]) -> None:
    """Assert T0's shape.

    A terminal on stdin is refused (64, the documented message); the redirect remedy runs.
    """
    if (
        probe["bare_exit"] != 64
        or "interactive terminal input is unsupported" not in probe["bare_stderr"]
    ):
        raise ShapeError(f"bare pty launch should be refused with 64, got {probe}")
    if probe["remedy_exit"] != 0:
        raise ShapeError(f"redirected stdin under the same pty should run, got {probe}")


def assert_resume(marker: Mapping[str, Any], c1_meta: Mapping[str, Any], c1_report: Report) -> None:
    """C2 proved the resume: the restored step is the saved step and the id matches C1's report."""
    if marker["restored_step"] != c1_meta["step"]:
        raise ShapeError(f"restored {marker['restored_step']} but C1 saved {c1_meta['step']}")
    if marker["resumed_from_request_id"] != c1_report["checkpoint"]["request_id"]:
        raise ShapeError("resume used a request_id the C1 report does not carry")
    if marker["final_step"] <= marker["restored_step"]:
        raise ShapeError("resumed run did not advance")


def redaction_clean(text: str, *, forbidden: tuple[str, ...]) -> list[str]:
    """Lines containing any forbidden fragment (home paths, login name, cache overrides)."""
    return [line for line in text.splitlines() if any(f in line for f in forbidden)]


def sample_windows_ms(report: Report) -> list[int]:
    """Every sample's ``window_ms``, in report order."""
    return [int(s["window_ms"]) for s in report["samples"]]


def percentile(values: Sequence[int], percent: int) -> int:
    """Nearest-rank percentile: sort ascending, index ``ceil(n * percent / 100) - 1``.

    Matches the nearest-rank definition ``reference_runtime_calibration.rs``'s ``percentile95``
    helper uses for the escalation-envelope measurements. Raises ``ValueError`` on an empty
    sequence — there is no percentile of nothing.
    """
    if not values:
        raise ValueError("percentile of an empty sequence")
    ordered = sorted(values)
    index = -(-(len(ordered) * percent) // 100) - 1
    return ordered[index]


def first_sample_captured_at_ms(report: Report) -> int | None:
    """Return the first sample's ``captured_at_ms``, or None when no sample was ever taken."""
    samples: Sequence[Mapping[str, Any]] = report["samples"]
    if not samples:
        return None
    return int(samples[0]["captured_at_ms"])


def intervention_overshoot(report: Report) -> dict[str, int | bool] | None:
    """Footprint observed at the moment of the first intervention signal, against the limit.

    None unless the outcome is a policy intervention with at least one signal and a resolved
    ``max_footprint_bytes`` (enforce mode only) — an observe report, or an uneventful exit, has
    nothing to measure an overshoot against. The observed value is the LAST sample with
    ``captured_at_ms <= signals[0].at_ms`` and an available footprint — never a later, post-signal
    sample (which would inflate the reading with footprint the policy never acted on), and never
    just the first post-signal sample either; this assumes samples arrive in non-decreasing
    ``captured_at_ms`` order, true of every real report. ``overshoot_bytes`` may be negative: a
    wall-time intervention can fire with the observed footprint still under the limit.
    """
    if report["outcome"]["kind"] != "policy_intervention":
        return None
    signals: Sequence[Mapping[str, Any]] = report["signals"]
    if not signals:
        return None
    configuration: Mapping[str, Any] = report["configuration"]
    limit_bytes = configuration.get("max_footprint_bytes")
    if limit_bytes is None:
        return None
    at_ms = signals[0]["at_ms"]

    observed: int | None = None
    for sample in report["samples"]:
        footprint = sample["aggregate_footprint_bytes"]
        if sample["captured_at_ms"] <= at_ms and footprint["status"] == "available":
            observed = int(footprint["value"])
    if observed is None:
        return None

    # `max_footprint_bytes` and `emergency_footprint_bytes` are a schema invariant: a valid
    # enforce-mode report always carries both together (report.rs's `mode_valid` check), so a
    # report with the former but not the latter is corrupt, not merely enforce-vs-observe. Read it
    # with `.get()` and decline to report an overshoot rather than KeyError on that corruption —
    # everything else about a malformed report is still left to raise.
    emergency_footprint_bytes = configuration.get("emergency_footprint_bytes")
    if emergency_footprint_bytes is None:
        return None

    limit_bytes = int(limit_bytes)
    emergency_threshold_bytes = int(emergency_footprint_bytes)
    return {
        "observed_at_intervention_bytes": observed,
        "limit_bytes": limit_bytes,
        "overshoot_bytes": observed - limit_bytes,
        "emergency_threshold_bytes": emergency_threshold_bytes,
        "within_band": observed < emergency_threshold_bytes,
    }


def sample_quality(report: Report) -> dict[str, int]:
    """Sample-window health: counts plus the p95/max window over EVERY sample.

    The percentile and max deliberately run over every sample's ``window_ms``, not only samples
    with an available footprint — a slow ``partial`` sample's window is exactly the signal 0081
    cares about, and restricting to available samples would hide it.
    """
    windows = sample_windows_ms(report)
    return {
        "sample_count": len(report["samples"]),
        "unavailable_samples": unavailable_count(report),
        "sample_window_p95_ms": percentile(windows, 95) if windows else 0,
        "sample_window_max_ms": max(windows) if windows else 0,
    }
