use std::time::Duration;

use mlx_guard_core::{
    ContainmentEvent, FootprintSampler, IdentityTracker, ObservationFailure,
    ObservationFailureKind, ProcessIdentity, ProcessObservation, ProcessSnapshot, SampleOutcome,
    SamplingClockError, SamplingConfig, SamplingConfigError, SnapshotError,
};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn identity(pid: i32, start_abstime: u64) -> ProcessIdentity {
    ProcessIdentity { pid, start_abstime }
}

fn observation(
    process: ProcessIdentity,
    parent_pid: i32,
    process_group_id: i32,
    footprint_bytes: Option<u64>,
) -> ProcessObservation {
    ProcessObservation {
        identity: process,
        parent_pid,
        process_group_id,
        footprint_bytes,
        exited: false,
    }
}

fn config(capacity: usize) -> SamplingConfig {
    SamplingConfig::new(ms(50), ms(100), ms(10), capacity).unwrap()
}

#[test]
fn sampling_configuration_rejects_unbounded_or_incoherent_values() {
    // Catches bypassing the interval and history ceilings in runtime callers.
    assert_eq!(
        SamplingConfig::new(ms(9), ms(100), ms(10), 4).unwrap_err(),
        SamplingConfigError::IntervalOutOfRange
    );
    assert_eq!(
        SamplingConfig::new(Duration::from_secs(11), ms(100), ms(10), 4).unwrap_err(),
        SamplingConfigError::IntervalOutOfRange
    );
    assert_eq!(
        SamplingConfig::new(ms(50), ms(10), ms(11), 4).unwrap_err(),
        SamplingConfigError::InvalidQualityWindow
    );
    assert_eq!(
        SamplingConfig::new(ms(50), ms(100), ms(10), 0).unwrap_err(),
        SamplingConfigError::HistoryCapacityOutOfRange
    );
    assert_eq!(
        SamplingConfig::new(ms(50), ms(100), ms(10), 4_097).unwrap_err(),
        SamplingConfigError::HistoryCapacityOutOfRange
    );
}

#[test]
fn complete_samples_preserve_window_members_and_feed_policy() {
    // Catches replacing a multi-call window with one timestamp or dropping per-process evidence.
    let root = identity(100, 1);
    let child = identity(101, 2);
    let tracker = IdentityTracker::new(root, 100).unwrap();
    let mut sampler = FootprintSampler::new(config(4), tracker);
    let sample = sampler
        .record_snapshot(
            ms(10),
            ms(15),
            Ok(ProcessSnapshot {
                observations: vec![
                    observation(root, 1, 100, Some(40)),
                    observation(child, 100, 100, Some(60)),
                ],
                failures: Vec::new(),
            }),
        )
        .clone();

    assert_eq!(sample.sequence, 0);
    assert_eq!(sample.started_at, ms(10));
    assert_eq!(sample.finished_at, ms(15));
    assert_eq!(sample.members.len(), 2);
    assert_eq!(sample.outcome, SampleOutcome::Complete { total_bytes: 100 });
    let event = sampler.policy_event(&sample, ms(20));
    assert_eq!(event.captured_at, ms(15));
    assert_eq!(event.processed_at, ms(20));
    assert_eq!(event.window, ms(5));
    assert_eq!(event.aggregate_bytes, Some(100));
    // Catches a projection that ignores age or accepts processing before capture.
    assert_eq!(sampler.policy_event(&sample, ms(116)).aggregate_bytes, None);
    assert_eq!(sampler.policy_event(&sample, ms(14)).aggregate_bytes, None);
}

#[test]
fn partial_error_and_zero_are_three_different_observations() {
    // Catches collapsing missing or failed observations into a real numeric zero.
    let root = identity(100, 1);
    let child = identity(101, 2);
    let tracker = IdentityTracker::new(root, 100).unwrap();
    let mut sampler = FootprintSampler::new(config(4), tracker);

    sampler.record_snapshot(
        ms(0),
        ms(1),
        Ok(ProcessSnapshot {
            observations: vec![
                observation(root, 1, 100, Some(0)),
                observation(child, 100, 100, Some(1)),
            ],
            failures: Vec::new(),
        }),
    );

    let partial = sampler
        .record_snapshot(
            ms(50),
            ms(51),
            Ok(ProcessSnapshot {
                observations: vec![observation(root, 1, 100, Some(0))],
                failures: vec![ObservationFailure {
                    pid: Some(child.pid),
                    kind: ObservationFailureKind::Unavailable,
                }],
            }),
        )
        .clone();
    assert!(matches!(
        partial.outcome,
        SampleOutcome::Partial { known_bytes: 0, ref missing_identities, ref observation_failures }
            if missing_identities == &[child]
                && observation_failures[0].kind == ObservationFailureKind::Unavailable
    ));
    assert_eq!(sampler.policy_event(&partial, ms(52)).aggregate_bytes, None);

    let failed = sampler
        .record_snapshot(
            ms(100),
            ms(101),
            Err(SnapshotError {
                kind: ObservationFailureKind::EnumerationFailed,
            }),
        )
        .clone();
    assert_eq!(
        failed.outcome,
        SampleOutcome::SnapshotFailed {
            kind: ObservationFailureKind::EnumerationFailed,
        }
    );
    assert_ne!(partial.outcome, failed.outcome);
}

#[test]
fn newly_listed_live_process_inspection_failure_keeps_sample_partial() {
    // Catches dropping a non-ESRCH failure merely because the PID has not been tracked before.
    let root = identity(100, 1);
    let tracker = IdentityTracker::new(root, 100).unwrap();
    let mut sampler = FootprintSampler::new(config(4), tracker);

    let sample = sampler
        .record_snapshot(
            ms(10),
            ms(11),
            Ok(ProcessSnapshot {
                observations: vec![observation(root, 1, 100, Some(10))],
                failures: vec![ObservationFailure {
                    pid: Some(101),
                    kind: ObservationFailureKind::PermissionDenied,
                }],
            }),
        )
        .clone();

    assert!(matches!(
        sample.outcome,
        SampleOutcome::Partial {
            known_bytes: 10,
            ref missing_identities,
            ref observation_failures,
        } if missing_identities.is_empty()
            && observation_failures == &[ObservationFailure {
                pid: Some(101),
                kind: ObservationFailureKind::PermissionDenied,
            }]
    ));
    assert_eq!(sampler.policy_event(&sample, ms(12)).aggregate_bytes, None);
}

#[test]
fn aggregate_overflow_never_reaches_policy_as_a_numeric_sample() {
    // Catches saturating or wrapping a multi-process sum into an enforcement input.
    let root = identity(100, 1);
    let child = identity(101, 2);
    let tracker = IdentityTracker::new(root, 100).unwrap();
    let mut sampler = FootprintSampler::new(config(4), tracker);
    let sample = sampler
        .record_snapshot(
            ms(10),
            ms(11),
            Ok(ProcessSnapshot {
                observations: vec![
                    observation(root, 1, 100, Some(u64::MAX)),
                    observation(child, 100, 100, Some(1)),
                ],
                failures: Vec::new(),
            }),
        )
        .clone();

    assert_eq!(sample.outcome, SampleOutcome::Overflow);
    assert_eq!(sampler.policy_event(&sample, ms(12)).aggregate_bytes, None);
}

#[test]
fn late_reversed_and_sleep_gap_samples_never_become_usable_data() {
    // Catches saturating a reversed clock into a valid sample or replaying missed intervals.
    let root = identity(100, 1);
    let tracker = IdentityTracker::new(root, 100).unwrap();
    let mut sampler = FootprintSampler::new(config(4), tracker);
    let late = sampler
        .record_snapshot(
            ms(10),
            ms(21),
            Ok(ProcessSnapshot {
                observations: vec![observation(root, 1, 100, Some(10))],
                failures: Vec::new(),
            }),
        )
        .clone();
    assert_eq!(sampler.policy_event(&late, ms(22)).aggregate_bytes, None);
    assert_eq!(sampler.delay_until_next(ms(60)).unwrap(), Duration::ZERO);

    let reversed = sampler
        .record_snapshot(
            ms(9),
            ms(8),
            Ok(ProcessSnapshot {
                observations: vec![observation(root, 1, 100, Some(u64::MAX))],
                failures: Vec::new(),
            }),
        )
        .clone();
    assert_eq!(reversed.outcome, SampleOutcome::ClockDiscontinuity);
    assert_eq!(sampler.policy_event(&reversed, ms(9)).aggregate_bytes, None);
    assert_eq!(
        sampler.delay_until_next(ms(9)).unwrap_err(),
        SamplingClockError
    );

    let zero_window = sampler
        .record_snapshot(
            ms(70),
            ms(70),
            Ok(ProcessSnapshot {
                observations: vec![observation(root, 1, 100, Some(10))],
                failures: Vec::new(),
            }),
        )
        .clone();
    assert_eq!(
        sampler.policy_event(&zero_window, ms(70)).aggregate_bytes,
        None
    );
}

#[test]
fn history_stays_fixed_capacity_during_churn_and_keeps_latest_evidence() {
    // Catches a Vec-backed history mutant whose memory grows with sampling duration.
    let root = identity(100, 1);
    let tracker = IdentityTracker::new(root, 100).unwrap();
    let mut sampler = FootprintSampler::new(config(3), tracker);
    for sequence in 0..10_000_u64 {
        let child = identity(101 + i32::try_from(sequence % 20).unwrap(), sequence + 2);
        sampler.record_snapshot(
            ms(sequence * 50),
            ms(sequence * 50 + 1),
            Ok(ProcessSnapshot {
                observations: vec![
                    observation(root, 1, 100, Some(1)),
                    observation(child, 100, 100, Some(1)),
                ],
                failures: Vec::new(),
            }),
        );
    }
    assert_eq!(sampler.history_len(), 3);
    assert_eq!(sampler.history_capacity(), 3);
    assert_eq!(
        sampler
            .history()
            .map(|sample| sample.sequence)
            .collect::<Vec<_>>(),
        vec![9_997, 9_998, 9_999]
    );
    assert!(sampler.history().all(|sample| {
        !sample.events.iter().any(|event| {
            matches!(
                event,
                ContainmentEvent::IdentityChanged { pid, .. } if *pid < 100
            )
        })
    }));
}
