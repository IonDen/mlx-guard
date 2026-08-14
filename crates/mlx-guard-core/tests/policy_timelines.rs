use std::time::Duration;

use mlx_guard_core::{
    Action, CheckpointDisposition, Event, PolicyConfig, PolicyMachine, PolicyState, SampleEvent,
    SignalNumber,
};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn config(checkpoint: bool) -> PolicyConfig {
    PolicyConfig {
        limit_bytes: 100,
        warning_bytes: 90,
        recovery_bytes: 80,
        emergency_bytes: 150,
        required_breach_samples: 2,
        max_missing_samples: 3,
        max_sample_age: ms(100),
        max_sample_window: ms(10),
        checkpoint_timeout: checkpoint.then(|| ms(50)),
        term_grace: ms(100),
        wall_time: None,
    }
}

fn sample(at_ms: u64, bytes: Option<u64>) -> Event {
    Event::Sample(SampleEvent {
        captured_at: ms(at_ms),
        processed_at: ms(at_ms),
        window: ms(1),
        aggregate_bytes: bytes,
    })
}

#[test]
fn observe_mode_never_turns_memory_into_a_signal() {
    // Catches observe mode accidentally sharing the destructive threshold path.
    let mut machine = PolicyMachine::observe(ms(100), ms(10), 3);
    for event in [sample(0, Some(u64::MAX)), sample(10, Some(u64::MAX))] {
        assert_eq!(machine.apply(event), [Action::RecordObservation]);
    }
    assert_eq!(machine.state(), PolicyState::Observe);
}

#[test]
fn warning_recovery_uses_hysteresis() {
    // Catches a no-hysteresis mutant that clears warning as soon as footprint falls below 90.
    let mut machine = PolicyMachine::enforce(config(false)).unwrap();
    machine.apply(sample(0, Some(95)));
    assert_eq!(machine.state(), PolicyState::Warning);
    machine.apply(sample(10, Some(85)));
    assert_eq!(machine.state(), PolicyState::Warning);
    machine.apply(sample(20, Some(80)));
    assert_eq!(machine.state(), PolicyState::Normal);
}

#[test]
fn two_fresh_breaches_request_checkpoint_and_false_ack_is_ignored() {
    // Catches single-sample action and a false-ack mutant that trusts signal receipt or wrong IDs.
    let mut machine = PolicyMachine::enforce(config(true)).unwrap();
    assert!(
        !machine
            .apply(sample(0, Some(100)))
            .iter()
            .any(Action::is_signal)
    );
    assert_eq!(machine.state(), PolicyState::Warning);
    assert_eq!(
        machine.apply(sample(10, Some(101))),
        [
            Action::RecordObservation,
            Action::RequestCheckpoint {
                request_id: 1,
                overshoot_bytes: 1,
                deadline_at: ms(60),
            },
        ]
    );
    assert_eq!(machine.state(), PolicyState::CheckpointRequested);

    for event in [
        Event::CheckpointAck {
            at: ms(20),
            request_id: 2,
            authenticated: true,
        },
        Event::CheckpointAck {
            at: ms(30),
            request_id: 1,
            authenticated: false,
        },
    ] {
        assert!(machine.apply(event).is_empty());
        assert_eq!(machine.state(), PolicyState::CheckpointRequested);
    }
    assert_eq!(
        machine.apply(Event::CheckpointAck {
            at: ms(40),
            request_id: 1,
            authenticated: true,
        }),
        [Action::SendTerm {
            checkpoint: CheckpointDisposition::AcknowledgedUnverifiedDurability,
        }]
    );
    assert_eq!(machine.state(), PolicyState::Terminating);
}

#[test]
fn runtime_can_seed_an_unpredictable_first_checkpoint_request_id() {
    // Catches reverting the wire-visible first request ID to the precomputable constant one.
    assert!(PolicyMachine::enforce_with_initial_request_id(config(true), 0).is_err());
    let mut machine =
        PolicyMachine::enforce_with_initial_request_id(config(true), 0x8ad4_32f1_905e_771b)
            .unwrap();
    machine.apply(sample(0, Some(100)));

    assert_eq!(
        machine.apply(sample(10, Some(101))),
        [
            Action::RecordObservation,
            Action::RequestCheckpoint {
                request_id: 0x8ad4_32f1_905e_771b,
                overshoot_bytes: 1,
                deadline_at: ms(60),
            },
        ]
    );
}

#[test]
fn checkpoint_timeout_and_term_grace_always_escalate() {
    // Catches a never-escalate mutant or a timeout treated as checkpoint success.
    let mut machine = PolicyMachine::enforce(config(true)).unwrap();
    machine.apply(sample(0, Some(100)));
    machine.apply(sample(10, Some(100)));
    assert_eq!(
        machine.apply(Event::Tick { at: ms(60) }),
        [Action::SendTerm {
            checkpoint: CheckpointDisposition::TimedOut,
        }]
    );
    assert_eq!(machine.state(), PolicyState::Terminating);
    assert_eq!(
        machine.apply(Event::Tick { at: ms(160) }),
        [Action::SendKill]
    );
    assert_eq!(machine.state(), PolicyState::Emergency);
}

#[test]
fn emergency_overshoot_skips_checkpoint_and_term() {
    // Catches an emergency path that waits through cooperative or TERM grace periods.
    let mut machine = PolicyMachine::enforce(config(true)).unwrap();
    assert_eq!(
        machine.apply(sample(0, Some(151))),
        [
            Action::RecordObservation,
            Action::SendKill,
            Action::RecordOvershoot { bytes: 51 },
        ]
    );
    assert_eq!(machine.state(), PolicyState::Emergency);
}

#[test]
fn stale_wide_and_missing_samples_fail_closed_only_in_enforcement() {
    // Catches treating missing data as zero or silently polling forever after capability loss.
    let stale = Event::Sample(SampleEvent {
        captured_at: ms(0),
        processed_at: ms(101),
        window: ms(1),
        aggregate_bytes: Some(1),
    });
    let wide = Event::Sample(SampleEvent {
        captured_at: ms(102),
        processed_at: ms(102),
        window: ms(11),
        aggregate_bytes: Some(1),
    });
    let missing = sample(103, None);

    let mut enforced = PolicyMachine::enforce(config(false)).unwrap();
    assert_eq!(enforced.apply(stale.clone()), [Action::RecordMissing]);
    assert_eq!(enforced.apply(wide.clone()), [Action::RecordMissing]);
    assert_eq!(
        enforced.apply(missing.clone()),
        [
            Action::RecordMissing,
            Action::SendTerm {
                checkpoint: CheckpointDisposition::SkippedObservationFailure,
            }
        ]
    );
    assert_eq!(enforced.state(), PolicyState::SupervisorError);

    let mut observed = PolicyMachine::observe(ms(100), ms(10), 3);
    observed.apply(stale);
    observed.apply(wide);
    assert_eq!(
        observed.apply(missing),
        [Action::RecordMissing, Action::StopObserving]
    );
    assert_eq!(observed.state(), PolicyState::SupervisorError);
}

#[test]
fn runtime_supervisor_fault_stops_observe_and_fails_closed_in_enforcement() {
    // Catches a post-launch platform fault returning before the owned command is made safe.
    let mut observed = PolicyMachine::observe(ms(100), ms(10), 3);
    assert_eq!(
        observed.apply(Event::SupervisorFault { at: ms(5) }),
        [Action::StopObserving]
    );
    assert_eq!(observed.state(), PolicyState::SupervisorError);

    let mut enforced = PolicyMachine::enforce(config(false)).unwrap();
    assert_eq!(
        enforced.apply(Event::SupervisorFault { at: ms(5) }),
        [Action::SendTerm {
            checkpoint: CheckpointDisposition::SkippedSupervisorFailure,
        }]
    );
    assert_eq!(enforced.state(), PolicyState::SupervisorError);
    assert_eq!(
        enforced.apply(Event::Tick { at: ms(105) }),
        [Action::SendKill]
    );
}

#[test]
fn wall_time_and_repeated_terminal_signal_have_deterministic_actions() {
    // Catches an ignored wall limit or repeated signal that waits through the grace period.
    let mut settings = config(false);
    settings.wall_time = Some(ms(50));
    let mut machine = PolicyMachine::enforce(settings).unwrap();
    assert_eq!(
        machine.apply(Event::Tick { at: ms(50) }),
        [Action::SendTerm {
            checkpoint: CheckpointDisposition::SkippedNotNegotiated,
        }]
    );

    let mut machine = PolicyMachine::enforce(config(false)).unwrap();
    let term = SignalNumber::new(15).unwrap();
    assert_eq!(
        machine.apply(Event::ExternalSignal {
            at: ms(1),
            signal: term
        }),
        [Action::ForwardSignal(term)]
    );
    assert_eq!(machine.state(), PolicyState::Terminating);
    assert_eq!(
        machine.apply(Event::ExternalSignal {
            at: ms(2),
            signal: term
        }),
        [Action::SendKill]
    );
    assert_eq!(machine.state(), PolicyState::Emergency);
}

#[test]
fn exit_report_counts_post_signal_observations_without_inventing_reclamation() {
    // Catches reporting signal delivery itself as observed process or Metal reclamation.
    let mut machine = PolicyMachine::enforce(config(false)).unwrap();
    machine.apply(sample(0, Some(100)));
    machine.apply(sample(10, Some(100)));
    assert_eq!(machine.state(), PolicyState::Terminating);
    machine.apply(sample(20, Some(12)));
    assert_eq!(
        machine.apply(Event::ProcessExited {
            at: ms(21),
            final_footprint_bytes: None,
        }),
        [Action::ReportExit {
            final_footprint_bytes: None,
            post_signal_observations: 1,
        }]
    );
    assert_eq!(machine.state(), PolicyState::Exited);
}

#[test]
fn invalid_policy_orderings_and_zero_timers_are_rejected() {
    // Catches accepting a policy whose thresholds cannot provide real hysteresis or escalation.
    let mut cases = Vec::new();

    let mut equal_recovery_and_warning = config(false);
    equal_recovery_and_warning.recovery_bytes = equal_recovery_and_warning.warning_bytes;
    cases.push(equal_recovery_and_warning);

    let mut emergency_not_above_limit = config(false);
    emergency_not_above_limit.emergency_bytes = emergency_not_above_limit.limit_bytes;
    cases.push(emergency_not_above_limit);

    let mut no_consecutive_breaches = config(false);
    no_consecutive_breaches.required_breach_samples = 0;
    cases.push(no_consecutive_breaches);

    let mut no_missing_budget = config(false);
    no_missing_budget.max_missing_samples = 0;
    cases.push(no_missing_budget);

    let mut zero_grace = config(false);
    zero_grace.term_grace = Duration::ZERO;
    cases.push(zero_grace);

    for invalid in cases {
        assert!(PolicyMachine::enforce(invalid).is_err());
    }
}

#[test]
fn clock_regression_fails_closed_and_still_escalates() {
    // Catches a monotonic-clock mutant that extends deadlines or silently reorders samples.
    let mut machine = PolicyMachine::enforce(config(false)).unwrap();
    machine.apply(sample(10, Some(1)));
    assert_eq!(
        machine.apply(sample(9, Some(1))),
        [
            Action::RecordMissing,
            Action::SendTerm {
                checkpoint: CheckpointDisposition::SkippedObservationFailure,
            },
        ]
    );
    assert_eq!(machine.state(), PolicyState::SupervisorError);
    assert_eq!(
        machine.apply(Event::Tick { at: ms(110) }),
        [Action::SendKill]
    );

    let mut observer = PolicyMachine::observe(ms(100), ms(10), 3);
    observer.apply(sample(10, Some(1)));
    assert_eq!(
        observer.apply(sample(9, Some(1))),
        [Action::RecordMissing, Action::StopObserving]
    );
}

#[test]
fn long_sleep_advances_one_safety_phase_per_observed_tick() {
    // Catches sleep recovery that emits checkpoint, TERM, and KILL from one stale wake-up.
    let mut settings = config(true);
    settings.wall_time = Some(ms(50));
    let mut machine = PolicyMachine::enforce(settings).unwrap();
    assert_eq!(
        machine.apply(Event::Tick { at: ms(500) }),
        [Action::RequestCheckpoint {
            request_id: 1,
            overshoot_bytes: 0,
            deadline_at: ms(550),
        }]
    );
    assert_eq!(
        machine.apply(Event::Tick { at: ms(1_000) }),
        [Action::SendTerm {
            checkpoint: CheckpointDisposition::TimedOut,
        }]
    );
    assert_eq!(
        machine.apply(Event::Tick { at: ms(1_100) }),
        [Action::SendKill]
    );
}
