#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_cli::{execute, parse_cli};
use mlx_guard_core::{
    CheckpointStatus, PolicyState, ReportV1, SignalResult, SignalTarget, SupervisorOutcome,
    TerminalKind, TerminalSignalMonitor, checkpoint_signal_usr1,
};

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
static RUNTIME_LOCK: Mutex<()> = Mutex::new(());

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-checkpoint-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn authenticated_checkpoint_precedes_term_in_the_complete_runtime() {
    // Catches runtime wiring that never negotiates the FD or mistakes signal delivery for an ack.
    let _runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let parsed = parse_cli([
        "mlx-guard",
        "run",
        "--max-footprint",
        "1TiB",
        "--wall-time",
        "1s",
        "--sample-interval",
        "10ms",
        "--report",
        report_path.to_str().unwrap(),
        "--",
        FIXTURE,
        "checkpoint-success",
        "1",
        "3000",
    ])
    .unwrap();

    let result = execute(parsed);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert!(result.stderr.is_empty());
    let report = ReportV1::from_json(&fs::read_to_string(report_path).unwrap()).unwrap();
    assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
    assert_eq!(
        report.checkpoint.status,
        CheckpointStatus::AcknowledgedUnverifiedDurability,
        "{report:#?}"
    );
    assert!(report.transitions.iter().any(|transition| {
        transition.from == PolicyState::Normal && transition.to == PolicyState::CheckpointRequested
    }));
    assert!(report.transitions.iter().any(|transition| {
        transition.from == PolicyState::CheckpointRequested
            && transition.to == PolicyState::Terminating
    }));
    assert!(report.signals.iter().any(|signal| {
        signal.signal == checkpoint_signal_usr1().get()
            && signal.target == SignalTarget::CooperativeEndpoint
            && signal.result == SignalResult::Delivered
    }));
    assert!(report.signals.iter().any(|signal| {
        signal.signal == 15
            && signal.target == SignalTarget::OwnedProcessGroup
            && signal.result == SignalResult::Delivered
    }));
}

#[test]
fn observe_finalizes_on_consecutive_missing_root_samples_without_signaling() {
    // Catches observe waiting forever or using cleanup-on-drop to kill after measurement loss.
    let _runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let parsed = parse_cli([
        "mlx-guard",
        "observe",
        "--sample-interval",
        "10ms",
        "--report",
        report_path.to_str().unwrap(),
        "--",
        FIXTURE,
        "fast-root-exit",
        "1",
        "500",
    ])
    .unwrap();
    let started = Instant::now();

    let result = execute(parsed);

    assert_eq!(result.outcome, SupervisorOutcome::SupervisorFailure);
    assert_eq!(result.stderr, "mlx-guard: footprint observation failed\n");
    assert!(started.elapsed() < Duration::from_millis(300));
    let report = ReportV1::from_json(&fs::read_to_string(report_path).unwrap()).unwrap();
    assert_eq!(report.outcome.kind, TerminalKind::SupervisorFailure);
    assert!(report.signals.is_empty());
    assert!(report.transitions.iter().any(|transition| {
        transition.from == PolicyState::Observe && transition.to == PolicyState::SupervisorError
    }));
    std::thread::sleep(Duration::from_millis(550));
}

#[test]
fn unavailable_supervisor_signal_control_finalizes_before_worker_launch() {
    // Catches a supervisor-owned resource failure escaping without a typed durable artifact.
    let _runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _occupied_monitor = TerminalSignalMonitor::install().unwrap();
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let marker = directory.0.join("worker-launched");
    let parsed = parse_cli([
        "mlx-guard",
        "run",
        "--max-footprint",
        "1TiB",
        "--report",
        report_path.to_str().unwrap(),
        "--",
        "/usr/bin/touch",
        marker.to_str().unwrap(),
    ])
    .unwrap();

    let result = execute(parsed);

    assert_eq!(result.outcome, SupervisorOutcome::SupervisorFailure);
    assert_eq!(
        result.stderr,
        "mlx-guard: a terminal signal monitor is already installed\n"
    );
    assert!(!marker.exists());
    let report = ReportV1::from_json(&fs::read_to_string(report_path).unwrap()).unwrap();
    assert_eq!(report.outcome.kind, TerminalKind::SupervisorFailure);
    assert!(report.samples.is_empty());
    assert!(report.signals.is_empty());
}

#[test]
fn checkpoint_timeout_cannot_delay_term_beyond_the_policy_deadline() {
    // Catches waiting for a blocked cooperative worker instead of enforcing the bounded timeout.
    let _runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let parsed = parse_cli([
        "mlx-guard",
        "run",
        "--max-footprint",
        "1TiB",
        "--wall-time",
        "1s",
        "--sample-interval",
        "10ms",
        "--report",
        report_path.to_str().unwrap(),
        "--",
        FIXTURE,
        "checkpoint-blocked",
        "500",
        "3000",
    ])
    .unwrap();

    let result = execute(parsed);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert!(result.stderr.is_empty());
    let report = ReportV1::from_json(&fs::read_to_string(report_path).unwrap()).unwrap();
    assert_eq!(report.checkpoint.status, CheckpointStatus::TimedOut);
    // The blocked worker sleeps 500 ms and then acknowledges (its 3 s argument is only its own
    // watchdog), so TERM must land within those 500 ms of the USR1 request — nominally after the
    // 100 ms checkpoint timeout — or the supervisor waited for the worker. Measuring the gap
    // between the two delivered signals keeps the check independent of how late a slow runner
    // reaches the 1 s wall deadline (an absolute at_ms bound flaked on macOS CI).
    let delivered_at = |number: u8, target: SignalTarget| {
        report
            .signals
            .iter()
            .find(|signal| {
                signal.signal == number
                    && signal.target == target
                    && signal.result == SignalResult::Delivered
            })
            .map(|signal| signal.at_ms)
    };
    let usr1_at = delivered_at(
        checkpoint_signal_usr1().get(),
        SignalTarget::CooperativeEndpoint,
    )
    .expect("checkpoint request must be delivered to the endpoint");
    let term_at = delivered_at(15, SignalTarget::OwnedProcessGroup)
        .expect("TERM must be delivered to the owned group");
    assert_eq!(report.checkpoint.at_ms, Some(term_at));
    assert!(
        term_at.saturating_sub(usr1_at) < 500,
        "TERM at {term_at} ms waited on the blocked worker after the request at {usr1_at} ms"
    );
}

#[test]
fn escaped_descendant_is_reported_as_containment_uncertainty() {
    // Catches presenting owned-group cleanup as complete containment after a descendant calls setsid.
    let _runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let parsed = parse_cli([
        "mlx-guard",
        "run",
        "--max-footprint",
        "1TiB",
        "--wall-time",
        "300ms",
        "--sample-interval",
        "10ms",
        "--report",
        report_path.to_str().unwrap(),
        "--",
        FIXTURE,
        "setsid-parent",
        "1",
        "800",
    ])
    .unwrap();

    let result = execute(parsed);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    let report = ReportV1::from_json(&fs::read_to_string(report_path).unwrap()).unwrap();
    assert_eq!(
        report.escape.detected,
        mlx_guard_core::Observed::Available { value: true }
    );
    assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
}
