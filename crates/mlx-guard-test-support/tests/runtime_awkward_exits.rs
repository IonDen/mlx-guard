#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use mlx_guard_cli::{RuntimeResult, execute, parse_cli};
use mlx_guard_core::{
    CheckpointStatus, ChildStatus, PolicyState, ReportV1, SignalReason, SupervisorOutcome,
    TerminalKind,
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
            "mlx-guard-awkward-{}-{timestamp}-{sequence}",
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

/// Supervise one fixture command and return both the process result and its final report.
fn run_report(extra: &[&str]) -> (RuntimeResult, ReportV1) {
    let _runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let mut arguments = vec![
        "mlx-guard",
        "run",
        "--max-footprint",
        "1TiB",
        "--sample-interval",
        "10ms",
        "--report",
        report_path.to_str().unwrap(),
    ];
    arguments.extend_from_slice(extra);
    let parsed = parse_cli(arguments).unwrap();

    let result = execute(parsed);

    let report = ReportV1::from_json(&fs::read_to_string(&report_path).unwrap()).unwrap();
    (result, report)
}

/// Return every recorded signal as its number and the reason the supervisor gave for it.
fn reasons(report: &ReportV1) -> Vec<(u8, Option<SignalReason>)> {
    report
        .signals
        .iter()
        .map(|signal| (signal.signal, signal.reason))
        .collect()
}

#[test]
fn root_exit_with_a_surviving_member_keeps_the_root_status_after_term_cleanup() {
    // Catches labelling survivor cleanup as a policy intervention or as measurement loss.
    let (result, report) = run_report(&["--", FIXTURE, "fast-root-exit", "1", "5000"]);

    assert_eq!(result.outcome, SupervisorOutcome::ChildExited(23));
    assert!(result.stderr.is_empty());
    assert_eq!(report.outcome.kind, TerminalKind::ChildExited { code: 23 });
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 23 })
    );
    assert!(
        report
            .transitions
            .iter()
            .all(|transition| transition.to != PolicyState::SupervisorError),
        "{report:#?}"
    );
    assert!(
        report
            .transitions
            .iter()
            .any(|transition| transition.to == PolicyState::Terminating),
        "{report:#?}"
    );
    // The cpu-stall survivor obeys TERM, so cleanup never has to escalate.
    assert_eq!(
        reasons(&report),
        [(15, Some(SignalReason::RootExitCleanup))],
        "{report:#?}"
    );
}

#[test]
fn root_exit_with_a_term_ignoring_survivor_escalates_to_kill_and_exit_75() {
    // Catches a KILL reached through the policy deadline that forgets it must own the result.
    let (result, report) = run_report(&["--", FIXTURE, "fast-root-exit", "2", "5000"]);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 23 })
    );
    assert_eq!(
        reasons(&report),
        [
            (15, Some(SignalReason::RootExitCleanup)),
            (9, Some(SignalReason::RootExitCleanup)),
        ],
        "{report:#?}"
    );
}

#[test]
fn exit_inside_the_term_grace_keeps_75_and_records_the_real_code() {
    // Catches dropping the child's own status once an intervention owns the exit code.
    let (result, report) = run_report(&[
        "--wall-time",
        "1s",
        "--",
        FIXTURE,
        "term-then-exit",
        "1",
        "5000",
    ]);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 3 })
    );
    // The worker never speaks the checkpoint protocol: request, unavailable, TERM, and no KILL.
    assert_eq!(
        reasons(&report),
        [(15, Some(SignalReason::WallTime))],
        "{report:#?}"
    );
    assert!(
        report.transitions.iter().any(|transition| {
            transition.from == PolicyState::Terminating && transition.to == PolicyState::Exited
        }),
        "{report:#?}"
    );
}

#[test]
fn exit_between_checkpoint_request_and_acknowledgement_is_truthful() {
    // Catches a request-then-exit being reported as a timeout or losing the child's status.
    // The 5 s acknowledgement window is the margin: the loop must observe the exit before it.
    let (result, report) = run_report(&[
        "--wall-time",
        "1s",
        "--checkpoint-timeout",
        "5s",
        "--",
        FIXTURE,
        "checkpoint-exit",
        "1",
        "5000",
    ]);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert_eq!(
        report.checkpoint.status,
        CheckpointStatus::RequestedUnverified
    );
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 0 })
    );
    assert!(
        reasons(&report)
            .iter()
            .all(|(signal, _)| *signal != 15 && *signal != 9),
        "{report:#?}"
    );
}

#[test]
fn exit_before_the_checkpoint_handshake_completes_keeps_the_child_status() {
    // Catches a never-completed handshake being reported as anything but the child's own exit.
    let (result, report) =
        run_report(&["--", FIXTURE, "checkpoint-exit-before-ready", "1", "5000"]);

    assert_eq!(result.outcome, SupervisorOutcome::ChildExited(0));
    assert_eq!(report.checkpoint.status, CheckpointStatus::NotNegotiated);
    assert!(report.signals.is_empty(), "{report:#?}");
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 0 })
    );
}

#[test]
fn a_hung_handshake_fails_the_checkpoint_request_and_terms_with_the_wall_reason() {
    // Catches a request against a never-ready channel blocking or being mislabelled.
    let (result, report) = run_report(&[
        "--wall-time",
        "1s",
        "--",
        FIXTURE,
        "checkpoint-hang-before-ready",
        "1",
        "5000",
    ]);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert_eq!(report.checkpoint.status, CheckpointStatus::NotNegotiated);
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Signaled { signal: 15 })
    );
    assert_eq!(
        reasons(&report),
        [(15, Some(SignalReason::WallTime))],
        "{report:#?}"
    );
}
