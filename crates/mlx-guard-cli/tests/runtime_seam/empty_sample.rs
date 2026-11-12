//! Unit tests for the runtime's gate between a sample and the policy: `sample_policy_event`.
//!
//! This file is not a test target of its own. `runtime.rs` includes it as a child module under
//! `#[cfg(test)]`, so the private gate is reached as `super::sample_policy_event`:
//!
//! ```text
//! #[cfg(test)]
//! #[path = "../tests/runtime_seam/empty_sample.rs"]
//! mod empty_sample_tests;
//! ```
//!
//! The gate's contract: `None` for a complete sample whose member set is empty (every process in
//! the group has exited, the root is not yet reaped, so nobody was measured), and the sampler's
//! own policy event for every other sample. Both `sample_once` functions feed the policy, and in
//! enforce mode the band, only when it returns an event.

use std::time::Duration;

use mlx_guard_core::{
    AdvisorySnapshot, BandEvent, BandWatch, Event, FootprintSample, FootprintSampler,
    IdentityTracker, ObservationFailure, ObservationFailureKind, ObserveCalibration, Observed,
    PolicyConfig, PolicyMachine, PolicyState, ProcessIdentity, ProcessObservation, ProcessSnapshot,
    SampleEvent, SampleOutcome, SamplingConfig, SnapshotError,
};

use super::{observed_value, sample_policy_event};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

const ROOT: ProcessIdentity = ProcessIdentity {
    pid: 100,
    start_abstime: 1,
};
const CHILD: ProcessIdentity = ProcessIdentity {
    pid: 101,
    start_abstime: 2,
};
const GROUP: i32 = 100;

/// A live member of the owned group.
fn live(
    process: ProcessIdentity,
    parent_pid: i32,
    footprint_bytes: Option<u64>,
) -> ProcessObservation {
    ProcessObservation {
        identity: process,
        parent_pid,
        process_group_id: GROUP,
        footprint_bytes,
        exited: false,
    }
}

/// Darwin's reading of a process that has exited but is not yet reaped: parent and group zero,
/// the footprint still reported, the exit flag set.
fn exited(process: ProcessIdentity, footprint_bytes: Option<u64>) -> ProcessObservation {
    ProcessObservation {
        identity: process,
        parent_pid: 0,
        process_group_id: 0,
        footprint_bytes,
        exited: true,
    }
}

fn snapshot(observations: Vec<ProcessObservation>) -> ProcessSnapshot {
    ProcessSnapshot {
        observations,
        failures: Vec::new(),
    }
}

/// Sampling quality bounds: interval 50 ms, a sample at most 100 ms old, its window at most
/// 10 ms. A window of 20 ms is therefore a missing sample.
fn sampler() -> FootprintSampler {
    FootprintSampler::new(
        SamplingConfig::new(ms(50), ms(100), ms(10), 8).unwrap(),
        IdentityTracker::new(ROOT, GROUP).unwrap(),
    )
}

/// Recovery 80, warning 90, limit 100, emergency 150 bytes; the third missing sample in a row
/// fails the run closed. The quality bounds match `sampler`.
fn policy() -> PolicyMachine {
    PolicyMachine::enforce(PolicyConfig {
        limit_bytes: 100,
        warning_bytes: 90,
        recovery_bytes: 80,
        emergency_bytes: 150,
        required_breach_samples: 2,
        max_missing_samples: 3,
        max_sample_age: ms(100),
        max_sample_window: ms(10),
        checkpoint_timeout: None,
        term_grace: ms(100),
        wall_time: None,
    })
    .unwrap()
}

/// One sample as the enforcing `sample_once` handles it: the policy, then the band, see the
/// sample only when the gate returns an event. The band reads the report window's footprint, as
/// the runtime does, not the policy's value: the two differ for a slow sample, which the policy
/// reads as missing while its window still holds the measured sum.
fn feed(
    machine: &mut PolicyMachine,
    band: &mut BandWatch,
    sample: &FootprintSample,
    event: Option<SampleEvent>,
) -> Option<BandEvent> {
    let event = event?;
    let at = event.processed_at;
    let advisory = AdvisorySnapshot::new(
        at,
        Observed::Unknown,
        Observed::Unknown,
        Observed::Unknown,
        Observed::Unknown,
        Observed::Unknown,
    );
    let window = ObserveCalibration::new().record_sample(sample, at, &advisory);
    machine.apply(Event::Sample(event));
    band.observe(
        machine.state(),
        observed_value(&window.aggregate_footprint_bytes),
        at,
    )
}

#[test]
fn an_empty_sample_after_a_warning_changes_nothing() {
    // Bug: the skip removed. The sample taken after the command exited but before it was
    // reaped sums an empty member set to 0 B, the policy reads that as a recovery
    // (Warning -> Normal at 0 B) and the band prints "warning cleared ... 0 B".
    let mut sampler = sampler();
    let mut machine = policy();
    let mut band = BandWatch::new();

    let in_band = sampler
        .record_snapshot(ms(10), ms(11), Ok(snapshot(vec![live(ROOT, 1, Some(95))])))
        .clone();
    let event = sample_policy_event(&sampler, &in_band, ms(12));
    assert_eq!(
        event.as_ref().map(|event| event.aggregate_bytes),
        Some(Some(95))
    );
    assert_eq!(
        feed(&mut machine, &mut band, &in_band, event),
        Some(BandEvent::Entered {
            footprint: 95,
            at: ms(12),
        })
    );
    assert_eq!(machine.state(), PolicyState::Warning);

    let after_exit = sampler
        .record_snapshot(ms(60), ms(61), Ok(snapshot(vec![exited(ROOT, Some(95))])))
        .clone();
    // The shape of the exit window, which the fix leaves to the sampler: nobody left to measure,
    // and a complete sum over nobody.
    assert!(after_exit.members.is_empty(), "{:?}", after_exit.members);
    assert_eq!(
        after_exit.outcome,
        SampleOutcome::Complete { total_bytes: 0 }
    );

    let event = sample_policy_event(&sampler, &after_exit, ms(62));
    assert_eq!(
        event, None,
        "a sample that measured nobody is not a reading"
    );
    assert_eq!(
        feed(&mut machine, &mut band, &after_exit, event),
        None,
        "no band line"
    );
    assert_eq!(
        machine.state(),
        PolicyState::Warning,
        "an exit is not a recovery"
    );
}

#[test]
fn an_empty_sample_after_two_missing_samples_never_fails_the_run_closed() {
    // Bug: the empty sample counted as missing (the empty member set made into "no value").
    // Two missing samples before it make it the third in a row: SupervisorError, TERM and exit
    // 70 for a command that ended on its own.
    let mut sampler = sampler();
    let mut machine = policy();
    let mut band = BandWatch::new();

    let normal = sampler
        .record_snapshot(ms(10), ms(11), Ok(snapshot(vec![live(ROOT, 1, Some(50))])))
        .clone();
    feed(
        &mut machine,
        &mut band,
        &normal,
        sample_policy_event(&sampler, &normal, ms(12)),
    );
    assert_eq!(machine.state(), PolicyState::Normal);

    // Two samples whose 20 ms windows exceed the 10 ms bound: missing, one and two in a row.
    for (start, finish) in [(60, 80), (110, 130)] {
        let slow = sampler
            .record_snapshot(
                ms(start),
                ms(finish),
                Ok(snapshot(vec![live(ROOT, 1, Some(50))])),
            )
            .clone();
        let event = sample_policy_event(&sampler, &slow, ms(finish + 1));
        assert_eq!(
            event.as_ref().map(|event| event.aggregate_bytes),
            Some(None),
            "a slow sample of a live root is a missing sample, not a skipped one"
        );
        feed(&mut machine, &mut band, &slow, event);
    }
    assert_eq!(machine.state(), PolicyState::Normal);

    let after_exit = sampler
        .record_snapshot(ms(160), ms(161), Ok(snapshot(vec![exited(ROOT, Some(50))])))
        .clone();
    assert!(after_exit.members.is_empty(), "{:?}", after_exit.members);
    feed(
        &mut machine,
        &mut band,
        &after_exit,
        sample_policy_event(&sampler, &after_exit, ms(162)),
    );
    assert_ne!(
        machine.state(),
        PolicyState::SupervisorError,
        "the empty sample must not be the third missing sample"
    );

    // The streak is still exactly two: one more missing sample is the third. A live child keeps
    // the probe's member set non-empty; its 20 ms window makes it missing. If the empty sample
    // had reset the streak (read as a 0 B reading), this would be only the first.
    let probe = sampler
        .record_snapshot(
            ms(210),
            ms(230),
            Ok(snapshot(vec![live(CHILD, 1, Some(30))])),
        )
        .clone();
    assert!(
        !probe.members.is_empty(),
        "the probe must measure the live child, or the claim below blames the wrong sample: {:?}",
        probe.members
    );
    feed(
        &mut machine,
        &mut band,
        &probe,
        sample_policy_event(&sampler, &probe, ms(231)),
    );
    assert_eq!(
        machine.state(),
        PolicyState::SupervisorError,
        "the missing streak was not left at two by the empty sample"
    );
}

#[test]
fn an_empty_sample_of_poor_quality_is_still_skipped() {
    // Bug: the skip kept only for a sample of valid quality (`&& quality_valid`). A slow or late
    // sample taken in the exit window then reaches the policy as a missing sample, and when it is
    // the third in a row a command that ended on its own is failed closed (TERM, exit 70).
    // Nobody was measured, whatever the sample's timing: it is skipped all the same.
    for (case, finished, processed) in [
        // A 20 ms window, past the 10 ms bound.
        ("slow", (160, 180), 181),
        // A 1 ms window processed 150 ms after it closed, past the 100 ms age bound.
        ("late", (160, 161), 311),
    ] {
        let mut sampler = sampler();
        sampler.record_snapshot(ms(10), ms(11), Ok(snapshot(vec![live(ROOT, 1, Some(95))])));
        let after_exit = sampler
            .record_snapshot(
                ms(finished.0),
                ms(finished.1),
                Ok(snapshot(vec![exited(ROOT, Some(95))])),
            )
            .clone();
        assert!(
            after_exit.members.is_empty(),
            "{case}: {:?}",
            after_exit.members
        );
        assert_eq!(
            after_exit.outcome,
            SampleOutcome::Complete { total_bytes: 0 },
            "{case}"
        );
        assert_eq!(
            sample_policy_event(&sampler, &after_exit, ms(processed)),
            None,
            "{case}: a sample that measured nobody is skipped whatever its quality"
        );
    }
}

#[test]
fn a_failed_enumeration_is_still_a_missing_sample() {
    // Bug: the skip keyed on an empty member set alone. A snapshot that could not list the
    // processes also has no members, so a supervisor that can no longer see the group would
    // skip every sample and never fail closed.
    let mut sampler = sampler();
    let failed = sampler
        .record_snapshot(
            ms(10),
            ms(11),
            Err(SnapshotError {
                kind: ObservationFailureKind::EnumerationFailed,
            }),
        )
        .clone();
    assert!(failed.members.is_empty());
    assert_eq!(
        sample_policy_event(&sampler, &failed, ms(12)),
        Some(SampleEvent {
            captured_at: ms(11),
            processed_at: ms(12),
            window: ms(1),
            aggregate_bytes: None,
        })
    );
}

#[test]
fn a_root_lost_before_its_exit_was_seen_is_still_a_missing_sample() {
    // Bug: the skip keyed on an empty member set alone. A root that vanished from the inventory
    // without an exit ever being observed leaves no member, but the sample is incomplete, not a
    // sum over nobody; skipping it hides the lost root from the missing-sample rule.
    let mut sampler = sampler();
    sampler.record_snapshot(ms(10), ms(11), Ok(snapshot(vec![live(ROOT, 1, Some(50))])));
    let lost = sampler
        .record_snapshot(
            ms(60),
            ms(61),
            Ok(ProcessSnapshot {
                observations: Vec::new(),
                failures: vec![ObservationFailure {
                    pid: Some(ROOT.pid),
                    kind: ObservationFailureKind::Disappeared,
                }],
            }),
        )
        .clone();
    assert!(lost.members.is_empty());
    assert!(
        matches!(lost.outcome, SampleOutcome::Partial { .. }),
        "{:?}",
        lost.outcome
    );
    assert_eq!(
        sample_policy_event(&sampler, &lost, ms(62)),
        Some(SampleEvent {
            captured_at: ms(61),
            processed_at: ms(62),
            window: ms(1),
            aggregate_bytes: None,
        })
    );
}

#[test]
fn a_clock_step_backwards_is_still_a_missing_sample() {
    // Bug: the skip keyed on an empty member set alone. A sample whose clock ran backwards
    // carries no members either; it must stay a missing sample.
    let mut sampler = sampler();
    let stepped = sampler
        .record_snapshot(ms(20), ms(10), Ok(snapshot(vec![live(ROOT, 1, Some(50))])))
        .clone();
    assert!(stepped.members.is_empty());
    assert_eq!(stepped.outcome, SampleOutcome::ClockDiscontinuity);
    assert_eq!(
        sample_policy_event(&sampler, &stepped, ms(12)),
        Some(SampleEvent {
            captured_at: ms(10),
            processed_at: ms(12),
            window: ms(0),
            aggregate_bytes: None,
        })
    );
}

#[test]
fn a_live_root_measured_at_zero_is_still_a_reading() {
    // Bug: the skip keyed on the value (a 0 B total) instead of on who was measured, so a real
    // zero reading from a live member is dropped.
    let mut sampler = sampler();
    let zero = sampler
        .record_snapshot(ms(10), ms(11), Ok(snapshot(vec![live(ROOT, 1, Some(0))])))
        .clone();
    assert_eq!(zero.members.len(), 1);
    assert_eq!(
        sample_policy_event(&sampler, &zero, ms(12)),
        Some(SampleEvent {
            captured_at: ms(11),
            processed_at: ms(12),
            window: ms(1),
            aggregate_bytes: Some(0),
        })
    );
}
