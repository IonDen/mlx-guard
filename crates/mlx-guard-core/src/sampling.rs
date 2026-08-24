use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::time::{Duration, Instant};

use crate::{
    AggregateFootprint, ContainmentEvent, IdentityTracker, NativeProcessInventory,
    ObservationFailure, ObservationFailureKind, ProcessIdentity, ProcessSnapshot, SampleEvent,
    SnapshotError,
};

/// Smallest supported interval between sample-window starts.
pub const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);

/// Largest supported interval between sample-window starts.
pub const MAX_SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// Maximum number of full samples retained in memory.
pub const MAX_SAMPLE_HISTORY_CAPACITY: usize = 4_096;

/// Validated bounds for one sampling loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SamplingConfig {
    interval: Duration,
    max_sample_age: Duration,
    max_sample_window: Duration,
    history_capacity: usize,
}

impl SamplingConfig {
    /// Validate loop timing and retained-history bounds.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unsupported interval, incoherent quality window, or unbounded
    /// history request.
    pub fn new(
        interval: Duration,
        max_sample_age: Duration,
        max_sample_window: Duration,
        history_capacity: usize,
    ) -> Result<Self, SamplingConfigError> {
        if !(MIN_SAMPLE_INTERVAL..=MAX_SAMPLE_INTERVAL).contains(&interval) {
            return Err(SamplingConfigError::IntervalOutOfRange);
        }
        if max_sample_window.is_zero()
            || max_sample_age.is_zero()
            || max_sample_window > max_sample_age
        {
            return Err(SamplingConfigError::InvalidQualityWindow);
        }
        if !(1..=MAX_SAMPLE_HISTORY_CAPACITY).contains(&history_capacity) {
            return Err(SamplingConfigError::HistoryCapacityOutOfRange);
        }
        Ok(Self {
            interval,
            max_sample_age,
            max_sample_window,
            history_capacity,
        })
    }

    #[must_use]
    pub const fn interval(self) -> Duration {
        self.interval
    }

    #[must_use]
    pub const fn max_sample_age(self) -> Duration {
        self.max_sample_age
    }

    #[must_use]
    pub const fn max_sample_window(self) -> Duration {
        self.max_sample_window
    }

    #[must_use]
    pub const fn history_capacity(self) -> usize {
        self.history_capacity
    }
}

/// Rejected sampler configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplingConfigError {
    IntervalOutOfRange,
    InvalidQualityWindow,
    HistoryCapacityOutOfRange,
}

impl fmt::Display for SamplingConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::IntervalOutOfRange => "sample interval is outside 10 ms through 10 s",
            Self::InvalidQualityWindow => "sample age and window bounds are incoherent",
            Self::HistoryCapacityOutOfRange => "sample history capacity is outside 1 through 4096",
        })
    }
}

impl Error for SamplingConfigError {}

/// A monotonic clock moved backwards relative to the previous sampling window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SamplingClockError;

impl fmt::Display for SamplingClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("monotonic sampling clock moved backwards")
    }
}

impl Error for SamplingClockError {}

/// One validated process contribution retained with its sample window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessFootprintSample {
    pub identity: ProcessIdentity,
    pub footprint_bytes: Option<u64>,
}

/// Aggregate result for one complete multi-process sampling attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SampleOutcome {
    Complete {
        total_bytes: u64,
    },
    Partial {
        known_bytes: u64,
        missing_identities: Vec<ProcessIdentity>,
        observation_failures: Vec<ObservationFailure>,
    },
    Overflow,
    SnapshotFailed {
        kind: ObservationFailureKind,
    },
    ClockDiscontinuity,
}

/// One bounded sample window and the evidence needed to interpret it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FootprintSample {
    pub sequence: u64,
    pub started_at: Duration,
    pub finished_at: Duration,
    pub members: Vec<ProcessFootprintSample>,
    pub outcome: SampleOutcome,
    pub events: Vec<ContainmentEvent>,
    pub escape_observed: bool,
}

/// Stateful conversion of native snapshots into a fixed-capacity sample history.
#[derive(Debug)]
pub struct FootprintSampler {
    config: SamplingConfig,
    tracker: IdentityTracker,
    history: VecDeque<FootprintSample>,
    next_sequence: u64,
    last_started_at: Option<Duration>,
}

impl FootprintSampler {
    #[must_use]
    pub fn new(config: SamplingConfig, tracker: IdentityTracker) -> Self {
        Self {
            config,
            tracker,
            history: VecDeque::with_capacity(config.history_capacity),
            next_sequence: 0,
            last_started_at: None,
        }
    }

    /// Read one native, non-atomic process snapshot and retain its bounded result.
    pub fn sample_native(
        &mut self,
        inventory: &NativeProcessInventory,
        run_started: Instant,
    ) -> &FootprintSample {
        let started_at = run_started.elapsed();
        let snapshot = inventory.snapshot_for_tracker(&self.tracker);
        let finished_at = run_started.elapsed();
        self.record_snapshot(started_at, finished_at, snapshot)
    }

    /// Convert an explicitly timed snapshot into retained evidence.
    ///
    /// Explicit timestamps keep clock-quality behavior deterministic and independently testable.
    ///
    /// # Panics
    ///
    /// Panics only if the validated positive history-capacity invariant is violated internally.
    pub fn record_snapshot(
        &mut self,
        started_at: Duration,
        finished_at: Duration,
        snapshot: Result<ProcessSnapshot, SnapshotError>,
    ) -> &FootprintSample {
        let clock_discontinuity = finished_at < started_at
            || self
                .last_started_at
                .is_some_and(|previous| started_at < previous);
        let (members, outcome, events, escape_observed) = if clock_discontinuity {
            (
                Vec::new(),
                SampleOutcome::ClockDiscontinuity,
                Vec::new(),
                false,
            )
        } else {
            self.last_started_at = Some(started_at);
            match snapshot {
                Ok(snapshot) => {
                    let frame = self.tracker.update(snapshot);
                    let members = frame
                        .owned_members
                        .iter()
                        .map(|member| ProcessFootprintSample {
                            identity: member.identity,
                            footprint_bytes: member.footprint_bytes,
                        })
                        .collect();
                    let outcome = match frame.aggregate_footprint {
                        AggregateFootprint::Complete(total_bytes)
                            if frame.observation_failures.is_empty() =>
                        {
                            SampleOutcome::Complete { total_bytes }
                        }
                        AggregateFootprint::Complete(total_bytes) => SampleOutcome::Partial {
                            known_bytes: total_bytes,
                            missing_identities: Vec::new(),
                            observation_failures: frame.observation_failures.clone(),
                        },
                        AggregateFootprint::Incomplete {
                            known_bytes,
                            missing_identities,
                        } => SampleOutcome::Partial {
                            known_bytes,
                            missing_identities,
                            observation_failures: frame.observation_failures.clone(),
                        },
                        AggregateFootprint::Overflow => SampleOutcome::Overflow,
                    };
                    (members, outcome, frame.events, frame.escape_observed)
                }
                Err(error) => (
                    Vec::new(),
                    SampleOutcome::SnapshotFailed { kind: error.kind },
                    Vec::new(),
                    false,
                ),
            }
        };
        let sample = FootprintSample {
            sequence: self.next_sequence,
            started_at,
            finished_at,
            members,
            outcome,
            events,
            escape_observed,
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        if self.history.len() == self.config.history_capacity {
            self.history.pop_front();
        }
        self.history.push_back(sample);
        self.history
            .back()
            .expect("a positive-capacity history contains the inserted sample")
    }

    /// Project one retained window into the policy input contract.
    #[must_use]
    pub fn policy_event(&self, sample: &FootprintSample, processed_at: Duration) -> SampleEvent {
        let window = sample.finished_at.saturating_sub(sample.started_at);
        let age = processed_at.saturating_sub(sample.finished_at);
        let clocks_valid = sample.finished_at >= sample.started_at
            && processed_at >= sample.finished_at
            && self
                .last_started_at
                .is_none_or(|last_started| sample.started_at <= last_started);
        let quality_valid = clocks_valid
            && !window.is_zero()
            && window <= self.config.max_sample_window
            && age <= self.config.max_sample_age;
        let aggregate_bytes = match sample.outcome {
            SampleOutcome::Complete { total_bytes } if quality_valid => Some(total_bytes),
            _ => None,
        };
        SampleEvent {
            captured_at: sample.finished_at,
            processed_at,
            window,
            aggregate_bytes,
        }
    }

    /// Return the delay to the next start without replaying intervals missed during sleep.
    ///
    /// # Errors
    ///
    /// Returns [`SamplingClockError`] when `now` precedes the last accepted window start.
    pub fn delay_until_next(&self, now: Duration) -> Result<Duration, SamplingClockError> {
        let Some(previous) = self.last_started_at else {
            return Ok(Duration::ZERO);
        };
        let elapsed = now.checked_sub(previous).ok_or(SamplingClockError)?;
        Ok(self.config.interval.saturating_sub(elapsed))
    }

    #[must_use]
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    #[must_use]
    pub const fn history_capacity(&self) -> usize {
        self.config.history_capacity
    }

    #[must_use]
    pub fn history(&self) -> impl ExactSizeIterator<Item = &FootprintSample> {
        self.history.iter()
    }

    /// Number of distinct identities observation has counted as having left the owned group.
    #[must_use]
    pub fn escaped_count(&self) -> u64 {
        self.tracker.escaped_count()
    }
}
