use std::time::Duration;

use mlx_guard_test_support::{
    FixtureLimitError, FixtureLimits, MAX_FIXTURE_ALLOCATION_BYTES, MAX_FIXTURE_WALL_TIME,
};

#[test]
fn accepts_the_exact_fixture_safety_ceiling() {
    // Catches changing an inclusive ceiling into an exclusive one.
    let limits = FixtureLimits::new(MAX_FIXTURE_ALLOCATION_BYTES, MAX_FIXTURE_WALL_TIME)
        .expect("the documented ceiling must be valid");

    assert_eq!(limits.allocation_bytes(), MAX_FIXTURE_ALLOCATION_BYTES);
    assert_eq!(limits.wall_time(), MAX_FIXTURE_WALL_TIME);
}

#[test]
fn rejects_zero_and_over_ceiling_allocations() {
    // Catches removing either the lower bound or the 128 MiB hard cap.
    assert_eq!(
        FixtureLimits::new(0, Duration::from_millis(1)),
        Err(FixtureLimitError::AllocationOutOfRange { requested: 0 })
    );
    assert_eq!(
        FixtureLimits::new(MAX_FIXTURE_ALLOCATION_BYTES + 1, Duration::from_millis(1),),
        Err(FixtureLimitError::AllocationOutOfRange {
            requested: MAX_FIXTURE_ALLOCATION_BYTES + 1,
        })
    );
}

#[test]
fn rejects_zero_and_over_ceiling_wall_times() {
    // Catches allowing an indefinite fixture or silently clamping an unsafe duration.
    assert_eq!(
        FixtureLimits::new(1, Duration::ZERO),
        Err(FixtureLimitError::WallTimeOutOfRange {
            requested: Duration::ZERO,
        })
    );
    let too_long = MAX_FIXTURE_WALL_TIME + Duration::from_millis(1);
    assert_eq!(
        FixtureLimits::new(1, too_long),
        Err(FixtureLimitError::WallTimeOutOfRange {
            requested: too_long,
        })
    );
}
