#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use mlx_guard_core::{
    Action, CheckpointBinding, CheckpointChannel, CheckpointNonce, CheckpointRejection,
    CheckpointSignalConfig, Event, InterventionEngine, LaunchOptions, OwnedProcess, PolicyConfig,
    PolicyMachine, PolicyState, ProcessInterventionActuator, RootOutcome, SampleEvent,
    SignalNumber, StdioMode,
};

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn fixture(mode: &str, value: u64, wall_ms: u64) -> LaunchOptions {
    LaunchOptions {
        command: vec![
            OsString::from(FIXTURE),
            OsString::from(mode),
            OsString::from(value.to_string()),
            OsString::from(wall_ms.to_string()),
        ],
        cwd: None,
        clear_env: false,
        env: BTreeMap::new(),
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    }
}

fn policy(checkpoint: bool) -> PolicyMachine {
    PolicyMachine::enforce(PolicyConfig {
        limit_bytes: 100,
        warning_bytes: 90,
        recovery_bytes: 80,
        emergency_bytes: 150,
        required_breach_samples: 2,
        max_missing_samples: 3,
        max_sample_age: ms(100),
        max_sample_window: ms(10),
        checkpoint_timeout: checkpoint.then(|| ms(50)),
        term_grace: ms(40),
        wall_time: None,
    })
    .unwrap()
}

fn sample(at_ms: u64, bytes: u64) -> Event {
    Event::Sample(SampleEvent {
        captured_at: ms(at_ms),
        processed_at: ms(at_ms),
        window: ms(1),
        aggregate_bytes: Some(bytes),
    })
}

fn signal(value: i32) -> SignalNumber {
    SignalNumber::new(u8::try_from(value).unwrap()).unwrap()
}

fn launch_checkpoint(mode: &str, value: u64) -> (OwnedProcess, CheckpointChannel) {
    let (mut channel, inherited) =
        CheckpointChannel::pair(CheckpointNonce::from_bytes([9; 32])).unwrap();
    let mut process =
        OwnedProcess::launch_with_checkpoint(&fixture(mode, value, 2_000), inherited).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    channel.begin_negotiation().unwrap();
    let deadline = Instant::now() + ms(500);
    while !channel.poll_ready().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(ms(1));
    }
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert!(line.starts_with("READY mode=checkpoint"));
    (process, channel)
}

fn wait_for_group_empty(process: &mut OwnedProcess) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while process.owned_group_exists().unwrap() && Instant::now() < deadline {
        let _ = process.try_wait_root().unwrap();
        std::thread::sleep(ms(1));
    }
    assert!(!process.owned_group_exists().unwrap());
}

#[test]
fn authenticated_checkpoint_acknowledgement_drives_term_against_the_group() {
    // Catches endpoint/group target inversion and signal delivery treated as checkpoint success.
    let (mut process, mut channel) = launch_checkpoint("checkpoint-success", 1);

    let endpoint = process
        .negotiate_checkpoint_endpoint(process.root_pid())
        .unwrap();
    let checkpoint_signal = CheckpointSignalConfig::new(signal(libc::SIGUSR1), ms(50)).unwrap();
    let binding = CheckpointBinding::new(&mut channel, endpoint, checkpoint_signal.signal());
    let mut actuator = ProcessInterventionActuator::new(&process, Some(binding));
    // Catches runtime composition assuming readiness without polling the authenticated channel.
    assert!(actuator.poll_checkpoint_ready().unwrap());
    let mut engine = InterventionEngine::new(policy(true), actuator);
    let _ = engine.handle(sample(0, 100));
    let decisions = engine.handle(sample(10, 101));
    assert!(matches!(
        decisions.last(),
        Some(Action::RequestCheckpoint { .. })
    ));

    let poll_deadline = Instant::now() + ms(200);
    let term_decisions = loop {
        let Some(event) = engine.actuator_mut().poll_checkpoint(ms(20)).unwrap().event else {
            assert!(Instant::now() < poll_deadline);
            std::thread::sleep(ms(1));
            continue;
        };
        break engine.handle(event);
    };
    assert!(matches!(
        term_decisions.as_slice(),
        [Action::SendTerm { .. }]
    ));
    drop(engine);
    wait_for_group_empty(&mut process);
    assert_eq!(
        process.wait_root().unwrap(),
        RootOutcome::Signaled(signal(libc::SIGTERM))
    );
}

#[test]
fn spoofed_acknowledgement_cannot_suppress_real_term() {
    // Catches forwarding any descriptor frame as an authenticated policy acknowledgement.
    let (mut process, mut channel) = launch_checkpoint("checkpoint-spoof", 1);
    let endpoint = process
        .negotiate_checkpoint_endpoint(process.root_pid())
        .unwrap();
    let binding = CheckpointBinding::new(&mut channel, endpoint, signal(libc::SIGUSR1));
    let actuator = ProcessInterventionActuator::new(&process, Some(binding));
    let mut engine = InterventionEngine::new(policy(true), actuator);
    let _ = engine.handle(sample(0, 100));
    let _ = engine.handle(sample(10, 101));

    let deadline = Instant::now() + ms(250);
    let mut saw_spoof = false;
    let term_decisions = loop {
        let observation = engine.actuator_mut().poll_checkpoint(ms(20)).unwrap();
        if observation
            .rejections
            .contains(&CheckpointRejection::WrongNonce)
        {
            saw_spoof = true;
            assert_eq!(observation.event, None);
            assert_eq!(engine.policy().state(), PolicyState::CheckpointRequested);
        }
        if let Some(event) = observation.event {
            break engine.handle(event);
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(ms(1));
    };
    assert!(saw_spoof);
    assert!(matches!(
        term_decisions.as_slice(),
        [Action::SendTerm { .. }]
    ));

    drop(engine);
    wait_for_group_empty(&mut process);
    assert_eq!(
        process.wait_root().unwrap(),
        RootOutcome::Signaled(signal(libc::SIGTERM))
    );
}

#[test]
fn blocked_checkpoint_cannot_extend_the_policy_deadline() {
    // Catches polling or worker progress extending the state-machine checkpoint timeout.
    let (mut process, mut channel) = launch_checkpoint("checkpoint-blocked", 150);
    let endpoint = process
        .negotiate_checkpoint_endpoint(process.root_pid())
        .unwrap();
    let binding = CheckpointBinding::new(&mut channel, endpoint, signal(libc::SIGUSR1));
    let actuator = ProcessInterventionActuator::new(&process, Some(binding));
    let mut engine = InterventionEngine::new(policy(true), actuator);
    let _ = engine.handle(sample(0, 100));
    let _ = engine.handle(sample(10, 101));
    assert_eq!(engine.policy().next_deadline(), Some(ms(60)));
    assert_eq!(
        engine.actuator_mut().poll_checkpoint(ms(20)).unwrap().event,
        None
    );

    let started = Instant::now();
    assert!(matches!(
        engine.handle(Event::Tick { at: ms(60) }).as_slice(),
        [Action::SendTerm { .. }]
    ));
    assert!(started.elapsed() < ms(10));

    drop(engine);
    wait_for_group_empty(&mut process);
    assert_eq!(
        process.wait_root().unwrap(),
        RootOutcome::Signaled(signal(libc::SIGTERM))
    );
}

#[test]
fn ignored_term_reaches_policy_deadline_then_kill_without_blocking_group_checks() {
    // Catches a blocking group check or an executor that lets TERM grace stall forever.
    let mut process = OwnedProcess::launch(&fixture("ignore-term", 1, 2_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert!(line.starts_with("READY mode=ignore-term"));

    let actuator = ProcessInterventionActuator::new(&process, None);
    let mut engine = InterventionEngine::new(policy(false), actuator);
    // Catches an actuator retaining a borrow that prevents the supervisor from polling root status.
    assert_eq!(process.try_wait_root().unwrap(), None);
    let _ = engine.handle(sample(0, 100));
    assert!(matches!(
        engine.handle(sample(10, 101)).as_slice(),
        [
            Action::RecordObservation,
            Action::SendTerm { .. },
            Action::RecordOvershoot { bytes: 1 },
        ]
    ));
    assert!(engine.actuator().owned_group_exists().unwrap());
    let checked_at = Instant::now();
    assert!(engine.actuator().owned_group_exists().unwrap());
    assert!(checked_at.elapsed() < ms(20));
    assert_eq!(engine.policy().next_deadline(), Some(ms(50)));
    assert_eq!(
        engine.handle(Event::Tick { at: ms(50) }),
        [Action::SendKill]
    );

    drop(engine);
    wait_for_group_empty(&mut process);
    assert_eq!(
        process.wait_root().unwrap(),
        RootOutcome::Signaled(signal(libc::SIGKILL))
    );
}

#[test]
fn emergency_sample_skips_checkpoint_and_term_in_the_real_adapter() {
    // Catches the real executor waiting through graceful stages after an emergency decision.
    let mut process = OwnedProcess::launch(&fixture("cpu-stall", 1, 2_000)).unwrap();
    let actuator = ProcessInterventionActuator::new(&process, None);
    let mut engine = InterventionEngine::new(policy(true), actuator);
    let decisions = engine.handle(sample(0, 151));
    assert_eq!(
        decisions,
        [
            Action::RecordObservation,
            Action::SendKill,
            Action::RecordOvershoot { bytes: 51 },
        ]
    );
    assert_eq!(engine.evidence().maximum_overshoot_bytes, 51);

    drop(engine);
    wait_for_group_empty(&mut process);
    assert_eq!(
        process.wait_root().unwrap(),
        RootOutcome::Signaled(signal(libc::SIGKILL))
    );
}

#[test]
fn threshold_decision_to_first_signal_p95_stays_within_ten_milliseconds() {
    // Catches synchronous work on the path between a policy decision and its first signal syscall.
    const REPETITIONS: usize = 32;
    let mut latencies = Vec::with_capacity(REPETITIONS);
    for _ in 0..REPETITIONS {
        let mut process = OwnedProcess::launch(&fixture("cpu-stall", 1, 2_000)).unwrap();
        let actuator = ProcessInterventionActuator::new(&process, None);
        let mut engine = InterventionEngine::new(policy(true), actuator);
        let started = Instant::now();
        let decisions = engine.handle(sample(0, 151));
        latencies.push(started.elapsed());
        assert!(decisions.contains(&Action::SendKill));
        drop(engine);
        wait_for_group_empty(&mut process);
        assert_eq!(
            process.wait_root().unwrap(),
            RootOutcome::Signaled(signal(libc::SIGKILL))
        );
    }
    latencies.sort_unstable();
    let p95 = latencies[(REPETITIONS * 95).div_ceil(100) - 1];
    eprintln!("threshold decision to first signal p95: {p95:?}");
    assert!(p95 <= ms(10), "first-signal p95 {p95:?} exceeded 10ms");
}
