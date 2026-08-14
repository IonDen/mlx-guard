//! Hard-bounded support for real-process acceptance fixtures.

use std::error::Error;
use std::fmt;
use std::time::Duration;

/// Maximum aggregate allocation a synthetic fixture may request.
pub const MAX_FIXTURE_ALLOCATION_BYTES: u64 = 128 * 1024 * 1024;

/// Maximum wall-clock duration accepted by a synthetic fixture.
pub const MAX_FIXTURE_WALL_TIME: Duration = Duration::from_secs(10);

/// Validated hard ceilings for a real-process fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixtureLimits {
    allocation_bytes: u64,
    wall_time: Duration,
}

impl FixtureLimits {
    /// Validate an allocation and wall-time request without clamping it.
    ///
    /// # Errors
    ///
    /// Returns [`FixtureLimitError`] when either value is zero or exceeds its hard ceiling.
    pub fn new(allocation_bytes: u64, wall_time: Duration) -> Result<Self, FixtureLimitError> {
        if allocation_bytes == 0 || allocation_bytes > MAX_FIXTURE_ALLOCATION_BYTES {
            return Err(FixtureLimitError::AllocationOutOfRange {
                requested: allocation_bytes,
            });
        }
        if wall_time.is_zero() || wall_time > MAX_FIXTURE_WALL_TIME {
            return Err(FixtureLimitError::WallTimeOutOfRange {
                requested: wall_time,
            });
        }
        Ok(Self {
            allocation_bytes,
            wall_time,
        })
    }

    /// Return the validated aggregate allocation ceiling.
    #[must_use]
    pub const fn allocation_bytes(self) -> u64 {
        self.allocation_bytes
    }

    /// Return the validated fixture wall-time ceiling.
    #[must_use]
    pub const fn wall_time(self) -> Duration {
        self.wall_time
    }
}

/// Reason a fixture request was rejected before doing work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixtureLimitError {
    /// The requested allocation was zero or exceeded the aggregate cap.
    AllocationOutOfRange { requested: u64 },
    /// The requested wall time was zero or exceeded the hard deadline.
    WallTimeOutOfRange { requested: Duration },
}

impl fmt::Display for FixtureLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocationOutOfRange { requested } => write!(
                formatter,
                "fixture allocation {requested} is outside 1..={MAX_FIXTURE_ALLOCATION_BYTES} bytes"
            ),
            Self::WallTimeOutOfRange { requested } => write!(
                formatter,
                "fixture wall time {requested:?} is outside (0, {MAX_FIXTURE_WALL_TIME:?}]"
            ),
        }
    }
}

impl Error for FixtureLimitError {}
