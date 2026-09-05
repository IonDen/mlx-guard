"""Validator tests. Each docstring names the one-line bug that turns the test red."""

import json
from pathlib import Path
from typing import Any

import pytest
import validators as v  # scripts/trial on sys.path via conftest.py

FIXTURES = Path(__file__).resolve().parents[3] / "crates/mlx-guard-core/tests/fixtures"


def load(name: str) -> dict[str, Any]:
    result: dict[str, Any] = json.loads((FIXTURES / name).read_text())
    return result


def emergency_report() -> dict[str, Any]:
    """Hand-built: the shape runtime.rs:828-840 + 1732-1734 produce for an emergency-band KILL."""
    report = load("report-v1.json")
    report["outcome"] = {
        "at_ms": 900,
        "kind": "policy_intervention",
        "final_footprint_bytes": {"status": "unknown"},
    }
    report["signals"] = [
        {
            "at_ms": 880,
            "signal": 9,
            "target": "owned_process_group",
            "result": "delivered",
            "reason": "footprint",
        }
    ]
    report["checkpoint"] = {"status": "not_negotiated"}
    report["transitions"] = [
        {"at_ms": 880, "from": "normal", "to": "emergency", "aggregate_footprint_bytes": 200}
    ]
    return report


def graceful_report() -> dict[str, Any]:
    """Hand-built from WRAP_A_COMMAND.md:149-175's real transcript."""
    report = load("report-v1.json")
    report["outcome"] = {
        "at_ms": 165,
        "kind": "policy_intervention",
        "final_footprint_bytes": {"status": "unknown"},
    }
    report["signals"] = [
        {
            "at_ms": 110,
            "signal": 15,
            "target": "owned_process_group",
            "result": "delivered",
            "reason": "footprint",
        }
    ]
    report["checkpoint"] = {"status": "not_negotiated", "reason": "footprint"}
    report["transitions"] = [
        {"at_ms": 60, "from": "normal", "to": "warning", "aggregate_footprint_bytes": 190}
    ]
    return report


def test_emergency_requires_reason_on_the_kill() -> None:
    """Bug: `signals[0].get("reason") is None` accepted — the 0055/0056 reason latch regressed \
unnoticed."""
    report = emergency_report()
    v.assert_emergency_footprint(report)
    del report["signals"][0]["reason"]
    with pytest.raises(v.ShapeError):
        v.assert_emergency_footprint(report)


def test_emergency_rejects_a_checkpoint_reason() -> None:
    """Bug: validator written from rev 1's text would accept `checkpoint.reason` on the \
emergency path."""
    report = emergency_report()
    report["checkpoint"]["reason"] = "footprint"
    with pytest.raises(v.ShapeError):
        v.assert_emergency_footprint(report)


def test_emergency_rejects_a_preceding_term() -> None:
    """Bug: `len(signals) >= 1` instead of `== 1` lets a TERM-then-KILL pass as emergency."""
    report = emergency_report()
    report["signals"].insert(
        0,
        {
            "at_ms": 800,
            "signal": 15,
            "target": "owned_process_group",
            "result": "delivered",
            "reason": "footprint",
        },
    )
    with pytest.raises(v.ShapeError):
        v.assert_emergency_footprint(report)


def test_graceful_rejects_a_request_id() -> None:
    """Bug (0063's box): a report carrying `request_id` passes the non-cooperative assertion."""
    report = graceful_report()
    v.assert_graceful_footprint(report)
    report["checkpoint"]["request_id"] = 7
    with pytest.raises(v.ShapeError):
        v.assert_graceful_footprint(report)


def test_graceful_rejects_an_emergency_transition() -> None:
    """Bug: band classification read from signals only — a KILL after TERM grace masked as \
graceful."""
    report = graceful_report()
    report["transitions"].append(
        {"at_ms": 120, "from": "warning", "to": "emergency", "aggregate_footprint_bytes": 400}
    )
    with pytest.raises(v.ShapeError):
        v.assert_graceful_footprint(report)


def test_graceful_allows_only_a_grace_expiry_kill_after_term() -> None:
    """Bug: any second signal accepted — a SIGUSR1 (30) would pass a non-cooperative run as \
graceful."""
    report = graceful_report()
    report["signals"].append(
        {
            "at_ms": 1110,
            "signal": 9,
            "target": "owned_process_group",
            "result": "delivered",
            "reason": "footprint",
        }
    )
    v.assert_graceful_footprint(report)
    report["signals"][1]["signal"] = 30
    with pytest.raises(v.ShapeError):
        v.assert_graceful_footprint(report)


def test_cooperative_shape_from_the_resume_fixture() -> None:
    """Bug: `request_id` compared truthy — `0` (the protocol's invalid id) would pass."""
    report = load("report-v1-resume.json")
    report["signals"] = [
        {
            "at_ms": 10,
            "signal": 30,
            "target": "cooperative_endpoint",
            "result": "delivered",
            "reason": "footprint",
        },
        {
            "at_ms": 13,
            "signal": 15,
            "target": "owned_process_group",
            "result": "delivered",
            "reason": "footprint",
        },
    ]
    assert v.assert_cooperative(report, reason="footprint")["request_id"] == 42
    report["checkpoint"]["request_id"] = 0
    with pytest.raises(v.ShapeError):
        v.assert_cooperative(report, reason="footprint")


def test_cooperative_requires_the_endpoint_signal_first() -> None:
    """Bug: signal order unchecked — TERM before SIGUSR1 (no negotiation happened) would pass."""
    report = load("report-v1-resume.json")
    report["signals"] = [
        {
            "at_ms": 13,
            "signal": 15,
            "target": "owned_process_group",
            "result": "delivered",
            "reason": "footprint",
        },
        {
            "at_ms": 14,
            "signal": 30,
            "target": "cooperative_endpoint",
            "result": "delivered",
            "reason": "footprint",
        },
    ]
    with pytest.raises(v.ShapeError):
        v.assert_cooperative(report, reason="footprint")


def test_uneventful_rejects_survivors_and_signals() -> None:
    """Bug: only `outcome.kind` checked — the awkward fixture (code 23, a cleanup TERM, \
survivors) would pass."""
    awkward = load("report-v1-awkward.json")
    with pytest.raises(v.ShapeError):
        v.assert_uneventful(awkward)
    with pytest.raises(v.ShapeError):
        v.assert_child_exit(
            awkward, 23
        )  # right code, but a signal record and survivors are present


def test_peak_filters_on_status() -> None:
    """Bug: `max(s["aggregate_footprint_bytes"]["value"])` KeyErrors or counts a partial \
subtotal as the peak."""
    report = load("report-v1.json")
    report["samples"].append(
        {
            "captured_at_ms": 20,
            "processed_at_ms": 21,
            "window_ms": 1,
            "aggregate_footprint_bytes": {"status": "partial", "known_subtotal": 10**9},
            "advisory": report["samples"][0]["advisory"],
        }
    )
    assert v.peak(report) == 90
    assert v.unavailable_count(report) == 1


def test_ring_wrapped_is_the_fifo_tell() -> None:
    """Bug: threshold `> 4096` — a full ring (exactly 4096) reads as not wrapped."""
    report = load("report-v1.json")
    report["samples"] = report["samples"] * 4096
    assert v.ring_wrapped(report)
    report["samples"].pop()
    assert not v.ring_wrapped(report)


def test_redaction_flags_home_and_login() -> None:
    """Bug: scanning for `/Users/` only — a bare login name in a log excerpt leaks."""
    assert v.redaction_clean("ok line\n", forbidden=("/Users/", "ionden")) == []
    assert v.redaction_clean("ran from ionden's box\n", forbidden=("/Users/", "ionden")) == [
        "ran from ionden's box"
    ]


def test_percentile_is_nearest_rank() -> None:
    """Bug: linear interpolation or an off-by-one index picks the wrong rank."""
    # nearest-rank p95 of 1..20 is the 2nd-largest, 19 — not an interpolated 19.05.
    assert v.percentile(list(range(1, 21)), 95) == 19
    assert v.percentile([5], 95) == 5
    with pytest.raises(ValueError):
        v.percentile([], 95)


def test_intervention_overshoot_uses_the_last_sample_before_the_signal() -> None:
    """Bug: uses the global peak (a later post-signal sample inflates the overshoot) or the \
first sample after the signal."""
    report = {
        "outcome": {"kind": "policy_intervention", "at_ms": 900},
        "signals": [
            {
                "at_ms": 880,
                "signal": 15,
                "target": "owned_process_group",
                "result": "delivered",
                "reason": "footprint",
            }
        ],
        "configuration": {"max_footprint_bytes": 100, "emergency_footprint_bytes": 150},
        "samples": [
            {
                "captured_at_ms": 800,
                "aggregate_footprint_bytes": {"status": "available", "value": 90},
            },
            {
                "captured_at_ms": 850,
                "aggregate_footprint_bytes": {"status": "available", "value": 95},
            },
            {
                "captured_at_ms": 880,  # equal to at_ms: still "before or at" the signal
                "aggregate_footprint_bytes": {"status": "available", "value": 98},
            },
            {
                "captured_at_ms": 920,  # after the signal: must never be used
                "aggregate_footprint_bytes": {"status": "available", "value": 500},
            },
        ],
    }
    assert v.intervention_overshoot(report) == {
        "observed_at_intervention_bytes": 98,
        "limit_bytes": 100,
        "overshoot_bytes": -2,
        "emergency_threshold_bytes": 150,
        "within_band": True,
    }


def test_intervention_overshoot_is_none_without_an_intervention_or_limit() -> None:
    """Bug: assumes `configuration.max_footprint_bytes` always exists — KeyErrors on an observe \
report."""
    observe_report = {
        "outcome": {"kind": "policy_intervention", "at_ms": 10},
        "signals": [
            {
                "at_ms": 10,
                "signal": 15,
                "target": "owned_process_group",
                "result": "delivered",
            }
        ],
        "configuration": {"sample_interval_ms": 50},  # observe mode: no max_footprint_bytes
        "samples": [],
    }
    assert v.intervention_overshoot(observe_report) is None

    uneventful_report = {
        "outcome": {"kind": "child_exited", "code": 0},
        "signals": [],
        "configuration": {"max_footprint_bytes": 100, "emergency_footprint_bytes": 150},
        "samples": [],
    }
    assert v.intervention_overshoot(uneventful_report) is None


def test_sample_quality_counts_unavailable_and_p95() -> None:
    """Bug: p95 computed over available samples only, hiding the slow partial samples that \
matter for 0081."""
    report = {
        "samples": [
            {"window_ms": 1, "aggregate_footprint_bytes": {"status": "available", "value": 10}},
            {"window_ms": 2, "aggregate_footprint_bytes": {"status": "available", "value": 20}},
            {
                "window_ms": 500,  # the slow one: only visible if partial samples aren't excluded
                "aggregate_footprint_bytes": {"status": "partial", "known_subtotal": 5},
            },
        ]
    }
    quality = v.sample_quality(report)
    assert quality == {
        "sample_count": 3,
        "unavailable_samples": 1,
        "sample_window_p95_ms": 500,
        "sample_window_max_ms": 500,
    }
