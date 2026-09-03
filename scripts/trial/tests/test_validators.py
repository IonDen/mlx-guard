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
