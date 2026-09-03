"""Band-prediction tests. Each docstring names the one-line bug that turns the test red."""


import bands as b


def test_late_peak_uses_the_tail() -> None:
    """Bug: `max(values)` instead of the last third (early load spike would set the limit above \
the steady state, so L3 never fires)."""
    values = [1000, 900, 800, 100, 105, 110, 95, 100, 105]
    assert b.late_peak(values) == 105


def test_derive_limit_floors() -> None:
    """Bug: rounding up puts the limit above P_late."""
    assert b.derive_limit(105) == 100
    assert b.derive_limit(104) == 99


def test_emergency_threshold_matches_the_cli_rule() -> None:
    """Bug: `limit * 11 // 10` differs from `limit + max(1, limit // 10)` for limits under 10 \
and for odd values."""
    assert b.emergency_threshold(100) == 110
    assert b.emergency_threshold(5) == 6


def test_largest_step_near_ignores_far_samples() -> None:
    """Bug: scanning the whole series lets a load-time GB jump predict emergency for a limit \
set at the steady tail."""
    limit = 100
    values = [0, 10**9, 500, 121, 96, 99, 101, 97]
    assert b.largest_step_near(values, limit) == 25


def test_predict_band_is_the_ten_percent_rule() -> None:
    """Bug: `<=` vs `<` at exactly `limit // 10` (a step equal to the band width crosses it)."""
    limit = 100
    values_at_threshold = [90, 100]  # step of exactly limit // 10 == 10
    assert b.predict_band(values_at_threshold, limit) == "emergency"
    values_below_threshold = [91, 100]  # step of 9 < 10
    assert b.predict_band(values_below_threshold, limit) == "graceful"


def test_headroom_limit() -> None:
    """Bug: float `peak * 1.25` yields a non-int the CLI grammar rejects."""
    result = b.headroom_limit(1000)
    assert result == 1250
    assert isinstance(result, int)
