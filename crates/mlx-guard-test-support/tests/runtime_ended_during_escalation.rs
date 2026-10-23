#![cfg(target_os = "macos")]

//! A command that ends while the supervisor is escalating must not turn the intervention that
//! ended it into a supervisor failure.
//!
//! Darwin answers a signal sent to a process group whose members have all exited, but are not yet
//! reaped, with EPERM rather than ESRCH. A worker that is still tearing down when TERM or KILL
//! reaches its group is in that state. The fixtures here park an exited member inside the owned
//! group for a bounded 100–300 ms, so the group stays in that state long enough for every
//! escalation step to meet it, instead of for the microseconds a real teardown race gives.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use mlx_guard_cli::{RuntimeResult, execute, parse_cli};
use mlx_guard_core::{
    CheckpointStatus, ChildStatus, PolicyState, ReportV1, SignalReason, SignalResult,
    SupervisorOutcome, TerminalKind, checkpoint_signal_usr1,
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
            "mlx-guard-ended-{}-{timestamp}-{sequence}",
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

/// Supervise one fixture under a one-second wall limit and return the result and its report.
fn run_under_wall_limit(command: &[&str]) -> (RuntimeResult, ReportV1) {
    let _runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let mut arguments = vec![
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
    ];
    arguments.extend_from_slice(command);
    let parsed = parse_cli(arguments).unwrap();

    let result = execute(parsed);

    let report = ReportV1::from_json(&fs::read_to_string(&report_path).unwrap()).unwrap();
    (result, report)
}

/// Return every recorded signal as its number, the reason given for it, and its result.
fn signals(report: &ReportV1) -> Vec<(u8, Option<SignalReason>, SignalResult)> {
    report
        .signals
        .iter()
        .map(|signal| (signal.signal, signal.reason, signal.result))
        .collect()
}

fn never_failed_closed(report: &ReportV1) -> bool {
    report
        .transitions
        .iter()
        .all(|transition| transition.to != PolicyState::SupervisorError)
}

#[test]
fn a_failed_acknowledgement_from_an_exiting_worker_keeps_the_intervention() {
    // Catches the TERM that answers a failed checkpoint being reported as a delivery failure
    // (exit 70, "signal delivery failed") when the worker's group has no live member left.
    let (result, report) =
        run_under_wall_limit(&[FIXTURE, "checkpoint-fail-linger", "300", "5000"]);

    assert_eq!(
        (result.outcome, result.stderr.as_str()),
        (SupervisorOutcome::PolicyIntervention, ""),
        "{report:#?}"
    );
    assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 1 })
    );
    assert!(never_failed_closed(&report), "{report:#?}");
    // The request reached the worker; its failed acknowledgement claims no saved state.
    assert_eq!(
        report.checkpoint.status,
        CheckpointStatus::RequestedUnverified,
        "{report:#?}"
    );
    assert_eq!(
        signals(&report),
        [
            (
                checkpoint_signal_usr1().get(),
                Some(SignalReason::WallTime),
                SignalResult::Delivered
            ),
            (
                15,
                Some(SignalReason::WallTime),
                SignalResult::ProcessMissing
            ),
        ],
        "{report:#?}"
    );
}

#[test]
fn a_terminal_signal_while_the_group_dies_does_not_fail_the_supervisor() {
    // Catches the KILL a second terminal event asks for being reported as a delivery failure when
    // the TERM before it already ended every live member of the group.
    let (result, report) = run_under_wall_limit(&[FIXTURE, "term-held-zombie", "300", "5000"]);

    assert_eq!(
        (result.outcome, result.stderr.as_str()),
        (SupervisorOutcome::PolicyIntervention, ""),
        "{report:#?}"
    );
    assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Signaled { signal: 15 })
    );
    assert!(never_failed_closed(&report), "{report:#?}");
    assert_eq!(
        signals(&report),
        [
            (15, Some(SignalReason::WallTime), SignalResult::Delivered),
            (
                9,
                Some(SignalReason::ExternalSignal),
                SignalResult::ProcessMissing
            ),
        ],
        "{report:#?}"
    );
}

#[test]
fn a_live_term_ignoring_group_still_escalates_to_a_delivered_kill() {
    // Negative control: catches an "already ended" reading leaking onto a group whose members are
    // alive and merely ignore TERM, which must still be escalated to a KILL that is delivered.
    let (result, report) = run_under_wall_limit(&[FIXTURE, "fanout-ignore-term", "2", "5000"]);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Signaled { signal: 9 })
    );
    assert!(never_failed_closed(&report), "{report:#?}");
    assert_eq!(
        signals(&report),
        [
            (15, Some(SignalReason::WallTime), SignalResult::Delivered),
            (9, Some(SignalReason::WallTime), SignalResult::Delivered),
        ],
        "{report:#?}"
    );
}
