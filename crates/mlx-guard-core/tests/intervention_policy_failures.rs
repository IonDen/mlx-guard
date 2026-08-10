use std::time::Duration;

use mlx_guard_core::{
    Action, ActuationFailure, ActuationKind, CheckpointDisposition, Event, PolicyConfig,
    PolicyMachine, PolicyState, SampleEvent,
};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn config() -> PolicyConfig {
    PolicyConfig {
        limit_bytes: 100,
        warning_bytes: 90,
        recovery_bytes: 80,
        emergency_bytes: 150,
        required_breach_samples: 2,
        max_missing_samples: 3,
        max_sample_age: ms(100),
        max_sample_window: ms(10),
        checkpoint_timeout: Some(ms(50)),
        term_grace: ms(100),
        wall_time: None,
    }
}

fn sample(at_ms: u64, bytes: u64) -> Event {
    Event::Sample(SampleEvent {
        captured_at: ms(at_ms),
        processed_at: ms(at_ms),
        window: ms(1),
        aggregate_bytes: Some(bytes),
    })
}

fn checkpoint_requested() -> PolicyMachine {
    let mut machine = PolicyMachine::enforce(config()).unwrap();
    let _ = machine.apply(sample(0, 100));
    let _ = machine.apply(sample(10, 101));
    machine
}

#[test]
fn checkpoint_decision_exposes_its_monotonic_deadline() {
    // Catches a runtime layer reconstructing or extending the state-machine deadline.
    let mut machine = PolicyMachine::enforce(config()).unwrap();
    let _ = machine.apply(sample(0, 100));
    assert_eq!(
        machine.apply(sample(10, 101)),
        [
            Action::RecordObservation,
            Action::RequestCheckpoint {
                request_id: 1,
                overshoot_bytes: 1,
                deadline_at: ms(60),
            },
        ]
    );
    assert_eq!(machine.next_deadline(), Some(ms(60)));
}

#[test]
fn checkpoint_actuation_failure_becomes_a_policy_decided_term() {
    // Catches an executor inventing a signal or waiting forever after channel failure.
    let mut machine = checkpoint_requested();
    assert_eq!(
        machine.apply(Event::ActuationFailed {
            at: ms(11),
            action: ActuationKind::Checkpoint,
            failure: ActuationFailure::CheckpointUnavailable,
        }),
        [Action::SendTerm {
            checkpoint: CheckpointDisposition::SkippedCheckpointFailure,
        }]
    );
    assert_eq!(machine.state(), PolicyState::Terminating);
    assert_eq!(machine.next_deadline(), Some(ms(111)));
}

#[test]
fn term_failure_escalates_and_kill_failure_is_a_terminal_supervisor_error() {
    // Catches retry loops that can stall forever after an operating-system signal error.
    let mut machine = checkpoint_requested();
    let _ = machine.apply(Event::ActuationFailed {
        at: ms(11),
        action: ActuationKind::Checkpoint,
        failure: ActuationFailure::CheckpointUnavailable,
    });
    assert_eq!(
        machine.apply(Event::ActuationFailed {
            at: ms(12),
            action: ActuationKind::Term,
            failure: ActuationFailure::PermissionDenied,
        }),
        [Action::SendKill]
    );
    assert_eq!(machine.state(), PolicyState::Emergency);
    assert_eq!(machine.next_deadline(), None);
    assert_eq!(
        machine.apply(Event::ActuationFailed {
            at: ms(13),
            action: ActuationKind::Kill,
            failure: ActuationFailure::SignalFailed,
        }),
        [Action::ReportSupervisorError {
            action: ActuationKind::Kill,
            failure: ActuationFailure::SignalFailed,
        }]
    );
    assert_eq!(machine.state(), PolicyState::SupervisorError);
}
