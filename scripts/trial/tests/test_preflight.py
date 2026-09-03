"""Preflight tests. Each docstring names the one-line bug that turns the test red."""

import preflight as p
import pytest


def test_parse_memory_pressure_free_percent_reads_the_named_line() -> None:
    """Bug: matching the wrong line (e.g. 'Pages free') instead of the named free-percentage \
line."""
    text = "Mach Virtual Memory Statistics: ...\nSystem-wide memory free percentage: 42%\n"
    assert p.parse_memory_pressure_free_percent(text) == 42.0


def test_parse_memory_pressure_free_percent_rejects_unrecognized_output() -> None:
    """Bug: silently returning 0 or 100 instead of failing loudly on unparseable output."""
    with pytest.raises(p.PreflightError):
        p.parse_memory_pressure_free_percent("unexpected output with no percentage line\n")


def test_is_on_battery_power_reads_only_the_first_line() -> None:
    """Bug: substring-matching the whole multi-line output, tripping on a battery mention \
further down."""
    ac = (
        "Now drawing from 'AC Power'\n"
        " -InternalBattery-0 (id=123)\t100%; charged; 0:00 remaining present: true"
    )
    battery = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=123)\t80%; discharging"
    assert p.is_on_battery_power(ac) is False
    assert p.is_on_battery_power(battery) is True


def test_check_gate_refuses_on_battery_even_with_ample_memory() -> None:
    """Bug: only checking free-memory percent, letting a battery-powered run through."""
    with pytest.raises(p.PreflightError):
        p.check_gate(free_percent=90.0, on_battery=True)


def test_check_gate_refuses_below_the_twenty_percent_floor() -> None:
    """Bug: `<=` vs `<` — exactly 20% free should pass, 19% must refuse."""
    p.check_gate(free_percent=20.0, on_battery=False)  # at the floor: passes
    with pytest.raises(p.PreflightError):
        p.check_gate(free_percent=19.0, on_battery=False)


def test_check_requirements_reports_only_absent_ones_with_their_size() -> None:
    """Bug: reporting every requirement's size regardless of whether it is already cached."""
    reqs = [
        p.ModelRequirement("cached-model", cache_check=lambda: True, download_size_bytes=1024**3),
        p.ModelRequirement(
            "missing-dataset", cache_check=lambda: False, download_size_bytes=2 * 1024**3
        ),
    ]
    messages = p.check_requirements(reqs)
    assert len(messages) == 1
    assert "missing-dataset" in messages[0]
    assert "2.0 GiB" in messages[0]


def test_scan_for_paths_flags_any_string_containing_a_slash() -> None:
    """Bug: only scanning top-level values, missing a path nested inside the ``binary`` \
sub-object."""
    clean = {
        "commit": "abc123",
        "binary": {"version": "0.1.0", "wheel_filename": "mlx_guard-0.1.0.whl"},
    }
    dirty = {"commit": "abc123", "binary": {"version": "0.1.0", "note": "/Users/ionden/trial"}}
    assert p.scan_for_paths(clean) == []
    assert p.scan_for_paths(dirty) == ["binary.note"]
