#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_cli::{execute, parse_cli};
use mlx_guard_core::{
    ArtifactKind, CheckpointArtifactRecord, CheckpointStatus, ChildStatus, PolicyState, ReportV1,
    SignalReason, SignalResult, SignalTarget, SupervisorOutcome, TerminalKind,
    TerminalSignalMonitor, checkpoint_signal_usr1, hangup_is_ignored,
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

/// Report whether the pid still names a process this test is allowed to signal.
fn process_exists(pid: i32) -> bool {
    // SAFETY: signal zero only probes the fixture pid's liveness and delivers nothing.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

/// Wait until the pid is gone, bounded by a deadline rather than a fixed sleep.
fn wait_until_gone(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!process_exists(pid));
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
    // Pins the shipped default: a real cooperative worker missed the former 100 ms bound on the
    // 3 vCPU CI runner, so the acknowledgement window defaults to one second (see docs/POLICY.md).
    assert_eq!(report.configuration.checkpoint_timeout_ms, Some(1_000));
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
    // The resume facts a worker needs to find its own saved state: the correlation key it echoed
    // and the limit that asked for the checkpoint. The 1 TiB footprint limit is unreachable, so
    // the wall deadline is the only thing that can have opened this intervention.
    assert!(
        report.checkpoint.request_id.is_some_and(|id| id != 0),
        "{report:#?}"
    );
    assert_eq!(
        report.checkpoint.reason,
        Some(SignalReason::WallTime),
        "{report:#?}"
    );
    // This worker acknowledges without artifact facts, so the report must not invent any.
    assert_eq!(report.checkpoint.artifact, None, "{report:#?}");
}

#[test]
fn a_worker_reported_artifact_reaches_the_report_verbatim() {
    // Catches artifact facts being dropped between the acknowledgement frame and the report, or
    // their kind and size being mangled on the way through.
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
        "checkpoint-artifact",
        "1",
        "3000",
    ])
    .unwrap();

    let result = execute(parsed);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert!(result.stderr.is_empty());
    let report = ReportV1::from_json(&fs::read_to_string(report_path).unwrap()).unwrap();
    assert_eq!(
        report.checkpoint.status,
        CheckpointStatus::AcknowledgedUnverifiedDurability,
        "{report:#?}"
    );
    assert_eq!(
        report.checkpoint.artifact,
        Some(CheckpointArtifactRecord {
            kind: ArtifactKind::File,
            size_bytes: Some(1_048_576),
        }),
        "{report:#?}"
    );
    assert!(
        report.checkpoint.request_id.is_some_and(|id| id != 0),
        "{report:#?}"
    );
}

#[test]
fn a_worker_that_never_negotiates_records_the_cause_without_a_request_id() {
    // The commonest real report: an ordinary command knows nothing about the protocol, so the
    // request fails before delivery. Catches a request id being latched for a request that never
    // reached an endpoint — a resuming worker would look for state no worker was ever asked to
    // save — while still naming the limit the supervisor acted on.
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
        "idle-stall",
        "1",
        "5000",
    ])
    .unwrap();

    let result = execute(parsed);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    let report = ReportV1::from_json(&fs::read_to_string(report_path).unwrap()).unwrap();
    assert_eq!(
        report.checkpoint.status,
        CheckpointStatus::NotNegotiated,
        "{report:#?}"
    );
    assert_eq!(report.checkpoint.request_id, None, "{report:#?}");
    assert_eq!(report.checkpoint.artifact, None, "{report:#?}");
    assert_eq!(
        report.checkpoint.reason,
        Some(SignalReason::WallTime),
        "{report:#?}"
    );
}

#[test]
fn observe_ends_at_root_exit_leaves_survivors_running_and_reports_it() {
    // Catches observe treating a root exit as measurement loss, or SIGKILLing the survivor on
    // drop instead of relinquishing it.
    let _runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let pid_file = directory.0.join("child.pid");
    let pid_file_assignment = format!("MLX_GUARD_FIXTURE_PID_FILE={}", pid_file.display());
    let parsed = parse_cli([
        "mlx-guard",
        "observe",
        "--sample-interval",
        "10ms",
        "--report",
        report_path.to_str().unwrap(),
        "--env",
        pid_file_assignment.as_str(),
        "--",
        FIXTURE,
        "fast-root-exit",
        "1",
        "5000",
    ])
    .unwrap();

    let result = execute(parsed);

    assert_eq!(result.outcome, SupervisorOutcome::ChildExited(23));
    assert_eq!(
        result.stderr,
        "mlx-guard: observe ended at root exit; owned-group members were still running and were \
         not signalled\n"
    );
    let report = ReportV1::from_json(&fs::read_to_string(report_path).unwrap()).unwrap();
    assert_eq!(report.outcome.kind, TerminalKind::ChildExited { code: 23 });
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 23 })
    );
    assert_eq!(report.outcome.owned_group_survivors, Some(true));
    assert!(report.signals.is_empty(), "{report:#?}");
    assert!(
        report
            .transitions
            .iter()
            .all(|transition| transition.to != PolicyState::SupervisorError),
        "{report:#?}"
    );
    let child: i32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal zero only probes the surviving fixture child and delivers nothing.
    assert_eq!(
        unsafe { libc::kill(child, 0) },
        0,
        "survivor must still be alive"
    );
    // SAFETY: the pid names this test's own fixture descendant, which the test now cleans up.
    unsafe { libc::kill(child, libc::SIGKILL) };
    wait_until_gone(child);
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
        "--checkpoint-timeout",
        "100ms",
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
    assert_eq!(report.configuration.checkpoint_timeout_ms, Some(100));
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
    // A request that was delivered and then timed out still names the state the worker was asked
    // to save, so a run that timed out can still be resumable. Catches the id being cleared when
    // the acknowledgement failed to arrive.
    assert!(
        report.checkpoint.request_id.is_some_and(|id| id != 0),
        "{report:#?}"
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

struct HangupDispositionGuard;
impl Drop for HangupDispositionGuard {
    fn drop(&mut self) {
        // SAFETY: restores the test-owned SIGHUP disposition even when an assertion panics.
        unsafe { libc::signal(libc::SIGHUP, libc::SIG_DFL) };
    }
}

#[test]
fn sighup_is_captured_unless_the_caller_already_ignored_it() {
    let _runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _guard = HangupDispositionGuard;
    // Phase 1: default disposition → captured and polled.
    {
        assert!(!hangup_is_ignored().unwrap());
        let monitor = TerminalSignalMonitor::install().unwrap();
        assert!(!monitor.hangup_ignored());
        // SAFETY: raising a captured signal in-process only exercises the installed handler.
        unsafe { libc::raise(libc::SIGHUP) };
        let signals = monitor.poll().unwrap();
        assert!(signals.iter().any(|s| s.get() == 1), "{signals:?}");
    }
    // Phase 2: SIG_IGN before install → probe reports it, capture skipped, drop preserves it.
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    {
        assert!(hangup_is_ignored().unwrap());
        let monitor = TerminalSignalMonitor::install().unwrap();
        assert!(monitor.hangup_ignored());
        unsafe { libc::raise(libc::SIGHUP) };
        assert!(monitor.poll().unwrap().is_empty());
    }
    assert!(
        hangup_is_ignored().unwrap(),
        "drop must not clobber the caller's SIG_IGN"
    );
}
