use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use mlx_guard_core::{
    Action, Actuation, ActuationFailure, ActuationKind, ActuationOutcome, Event,
    InterventionActuator, InterventionEngine, InterventionProgress, MAX_INTERVENTION_RECORDS,
    PolicyConfig, PolicyMachine, PolicyState, SampleEvent,
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

fn sample(at_ms: u64, bytes: u64) -> Event {
    Event::Sample(SampleEvent {
        captured_at: ms(at_ms),
        processed_at: ms(at_ms),
        window: ms(1),
        aggregate_bytes: Some(bytes),
    })
}

#[derive(Default)]
struct FakeActuator {
    attempted: Vec<Actuation>,
    results: VecDeque<Result<ActuationOutcome, ActuationFailure>>,
}

impl InterventionActuator for FakeActuator {
    fn execute(
        &mut self,
        _requested_at: Duration,
        action: Actuation,
    ) -> Result<ActuationOutcome, ActuationFailure> {
        self.attempted.push(action);
        self.results
            .pop_front()
            .unwrap_or(Ok(ActuationOutcome::Delivered))
    }
}

struct OrderingActuator {
    events: Rc<RefCell<Vec<&'static str>>>,
}

impl InterventionActuator for OrderingActuator {
    fn execute(
        &mut self,
        _requested_at: Duration,
        _action: Actuation,
    ) -> Result<ActuationOutcome, ActuationFailure> {
        self.events.borrow_mut().push("actuation");
        Ok(ActuationOutcome::Delivered)
    }
}

#[test]
fn transition_observer_runs_before_the_policy_actuation() {
    // Catches delivering a destructive signal before the decisive transition can be persisted.
    let events = Rc::new(RefCell::new(Vec::new()));
    let actuator = OrderingActuator {
        events: Rc::clone(&events),
    };
    let mut settings = config(false);
    settings.wall_time = Some(ms(10));
    let policy = PolicyMachine::enforce(settings).unwrap();
    let mut engine = InterventionEngine::new(policy, actuator);
    let observer_events = Rc::clone(&events);

    let actions =
        engine.handle_with_transition_observer(Event::Tick { at: ms(10) }, move |at, from, to| {
            assert_eq!(at, ms(10));
            assert_eq!(from, PolicyState::Normal);
            assert_eq!(to, PolicyState::Terminating);
            observer_events.borrow_mut().push("transition");
        });

    assert!(matches!(actions.as_slice(), [Action::SendTerm { .. }]));
    assert_eq!(*events.borrow(), ["transition", "actuation"]);
}

#[test]
fn driver_executes_only_policy_returned_actions_and_keeps_policy_deadline() {
    // Catches an executor that reconstructs a timeout or sends TERM directly on a warning.
    let policy = PolicyMachine::enforce(config(true)).unwrap();
    let mut engine = InterventionEngine::new(policy, FakeActuator::default());

    let first = engine.handle(sample(0, 100));
    assert!(first.iter().all(|action| !action.is_signal()));
    assert!(engine.actuator().attempted.is_empty());

    let second = engine.handle(sample(10, 101));
    let checkpoint = Actuation::Checkpoint {
        request_id: 1,
        overshoot_bytes: 1,
        deadline_at: ms(60),
    };
    assert!(second.contains(&Action::RequestCheckpoint {
        request_id: 1,
        overshoot_bytes: 1,
        deadline_at: ms(60),
    }));
    assert_eq!(engine.actuator().attempted, [checkpoint]);
    assert_eq!(engine.policy().contract_version(), 1);
    assert_eq!(engine.evidence().policy_contract_version, 1);
    assert_eq!(engine.policy().next_deadline(), Some(ms(60)));
    assert_eq!(
        engine.observe_group_status(ms(11), true),
        InterventionProgress::ObserveUntil {
            deadline_at: Some(ms(60)),
        }
    );
    assert_eq!(engine.evidence().maximum_overshoot_bytes, 1);
}

#[test]
fn platform_failure_chain_is_finite_and_entirely_policy_decided() {
    // Catches an executor retry loop or one that silently treats a failed signal as delivered.
    let actuator = FakeActuator {
        attempted: Vec::new(),
        results: VecDeque::from([
            Err(ActuationFailure::CheckpointUnavailable),
            Err(ActuationFailure::PermissionDenied),
            Err(ActuationFailure::SignalFailed),
        ]),
    };
    let policy = PolicyMachine::enforce(config(true)).unwrap();
    let mut engine = InterventionEngine::new(policy, actuator);
    let _ = engine.handle(sample(0, 100));
    let actions = engine.handle(sample(10, 101));

    assert_eq!(
        engine
            .actuator()
            .attempted
            .iter()
            .map(Actuation::kind)
            .collect::<Vec<_>>(),
        [
            ActuationKind::Checkpoint,
            ActuationKind::Term,
            ActuationKind::Kill,
        ]
    );
    assert!(matches!(
        actions.last(),
        Some(Action::ReportSupervisorError {
            action: ActuationKind::Kill,
            failure: ActuationFailure::SignalFailed,
        })
    ));
    assert_eq!(engine.policy().state(), PolicyState::SupervisorError);
    assert_eq!(engine.evidence().records().len(), 3);
    assert_eq!(
        engine.observe_group_status(ms(11), true),
        InterventionProgress::TerminalSupervisorError
    );
}

#[test]
fn successful_action_requires_later_observation_before_decrease_is_recorded() {
    // Catches counting signal delivery, or the threshold sample itself, as memory reclamation.
    let policy = PolicyMachine::enforce(config(false)).unwrap();
    let mut engine = InterventionEngine::new(policy, FakeActuator::default());
    let _ = engine.handle(sample(0, 100));
    let _ = engine.handle(sample(10, 101));
    assert_eq!(engine.evidence().maximum_overshoot_bytes, 1);

    let record = &engine.evidence().records()[0];
    assert_eq!(record.first_observation_latency, None);
    assert_eq!(record.observed_footprint_decrease_latency, None);

    let _ = engine.handle(sample(20, 101));
    let record = &engine.evidence().records()[0];
    assert_eq!(record.first_observation_latency, Some(ms(10)));
    assert_eq!(record.observed_footprint_decrease_latency, None);

    let _ = engine.handle(sample(30, 90));
    let record = &engine.evidence().records()[0];
    assert_eq!(record.observed_footprint_decrease_latency, Some(ms(20)));

    assert_eq!(
        engine.observe_group_status(ms(40), false),
        InterventionProgress::GroupEmpty
    );
    assert_eq!(
        engine.evidence().records()[0].group_empty_latency,
        Some(ms(30))
    );
}

#[test]
fn invalid_sample_does_not_satisfy_post_action_observation() {
    // Catches recording a stale or failed sample as evidence of post-signal process state.
    let policy = PolicyMachine::enforce(config(false)).unwrap();
    let mut engine = InterventionEngine::new(policy, FakeActuator::default());
    let _ = engine.handle(sample(0, 100));
    let _ = engine.handle(sample(10, 101));
    let stale = Event::Sample(SampleEvent {
        captured_at: ms(0),
        processed_at: ms(200),
        window: ms(1),
        aggregate_bytes: Some(1),
    });
    assert_eq!(engine.handle(stale), [Action::RecordMissing]);
    assert_eq!(
        engine.evidence().records()[0].first_observation_latency,
        None
    );

    let _ = engine.handle(sample(210, 101));
    assert_eq!(
        engine.evidence().records()[0].first_observation_latency,
        Some(ms(200))
    );
}

#[test]
fn evidence_is_bounded_while_aggregate_counts_remain_complete() {
    // Catches a long-running supervisor retaining one heap record per repeated signal.
    let policy = PolicyMachine::enforce(config(false)).unwrap();
    let mut engine = InterventionEngine::new(policy, FakeActuator::default());
    let _ = engine.handle(sample(0, 151));
    for at in 1..=(MAX_INTERVENTION_RECORDS as u64 + 10) {
        let _ = engine.handle(Event::Tick { at: ms(at) });
        let _ = engine.handle(Event::ExternalSignal {
            at: ms(at + 100),
            signal: mlx_guard_core::SignalNumber::new(15).unwrap(),
        });
    }

    assert_eq!(engine.evidence().records().len(), MAX_INTERVENTION_RECORDS);
    assert!(engine.evidence().total_attempts > MAX_INTERVENTION_RECORDS as u64);
    assert_eq!(
        engine.evidence().dropped_records,
        engine.evidence().total_attempts - MAX_INTERVENTION_RECORDS as u64
    );
    assert_eq!(engine.evidence().maximum_overshoot_bytes, 51);
}

#[test]
fn bounded_ramp_records_sampled_overshoot_instead_of_assuming_the_limit() {
    // Catches reporting threshold termination without the sampled amount above that threshold.
    const MIB: u64 = 1024 * 1024;
    let mut settings = config(false);
    settings.recovery_bytes = 48 * MIB;
    settings.warning_bytes = 56 * MIB;
    settings.limit_bytes = 64 * MIB;
    settings.emergency_bytes = 128 * MIB;
    let policy = PolicyMachine::enforce(settings).unwrap();
    let mut engine = InterventionEngine::new(policy, FakeActuator::default());

    let _ = engine.handle(sample(0, 64 * MIB));
    let decisions = engine.handle(sample(50, 71 * MIB));
    assert!(decisions.contains(&Action::RecordOvershoot { bytes: 7 * MIB }));
    assert_eq!(engine.evidence().maximum_overshoot_bytes, 7 * MIB);
}
