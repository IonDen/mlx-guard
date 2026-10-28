#![cfg(target_os = "macos")]

//! A command that ends while the supervisor is escalating must not turn the intervention that
//! ended it into a supervisor failure.
//!
//! Darwin answers a signal sent to a process group whose members have all exited, but are not yet
//! reaped, with EPERM rather than ESRCH. A worker that is still tearing down when TERM or KILL
//! reaches its group is in that state. The fixtures here park an exited member inside the owned
//! group and keep it there until the supervisor's journal records the signal under test, so that
//! signal is known to have met the group in that state. Without that, a group that had already
//! gone away would answer ESRCH, which also reads as `process_missing`, and the test would pass
//! without exercising anything. The fixture's wall-time watchdog bounds the hold.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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

/// A file that exists only while the in-process supervisor runs.
///
/// The zombie holder interrupts the supervisor, which is this test process, only while the
/// marker exists, so a run that went wrong cannot interrupt a later test.
struct InterruptMarker(PathBuf);

impl InterruptMarker {
    fn create(path: PathBuf) -> Self {
        fs::write(&path, b"").unwrap();
        Self(path)
    }
}

impl Drop for InterruptMarker {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// One supervised run and, for a held group, what the zombie holder recorded on release.
struct Run {
    result: RuntimeResult,
    report: ReportV1,
    /// `None` when the holder never saw its signal recorded and its watchdog ended the hold.
    hold_record: Option<String>,
}

/// Supervise one fixture under a one-second wall limit.
fn run_under_wall_limit(command: &[&str]) -> Run {
    let _runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let hold_record = directory.0.join("hold-record");
    let journal_env = env_assignment(
        "MLX_GUARD_FIXTURE_RELEASE_JOURNAL",
        &directory.0.join(".report.json.journal"),
    );
    let record_env = env_assignment("MLX_GUARD_FIXTURE_HOLD_RECORD", &hold_record);
    let marker_path = directory.0.join("interrupt-allowed");
    let marker_env = env_assignment("MLX_GUARD_FIXTURE_INTERRUPT_MARKER", &marker_path);
    let mut arguments = vec![
        "mlx-guard",
        "run",
        "--max-footprint",
        "1TiB",
        "--wall-time",
        "1s",
        "--sample-interval",
        "10ms",
        "--env",
        &journal_env,
        "--env",
        &record_env,
        "--env",
        &marker_env,
        "--report",
        report_path.to_str().unwrap(),
        "--",
    ];
    arguments.extend_from_slice(command);
    let parsed = parse_cli(arguments).unwrap();

    let marker = InterruptMarker::create(marker_path);
    let result = execute(parsed);
    drop(marker);

    let report = ReportV1::from_json(&fs::read_to_string(&report_path).unwrap()).unwrap();
    Run {
        result,
        report,
        hold_record: fs::read_to_string(&hold_record).ok(),
    }
}

fn env_assignment(name: &str, path: &Path) -> String {
    format!("{name}={}", path.to_str().unwrap())
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
    let Run {
        result,
        report,
        hold_record,
    } = run_under_wall_limit(&[FIXTURE, "checkpoint-fail-linger", "1", "5000"]);

    assert_eq!(
        hold_record.as_deref(),
        Some("released after signal 15 was recorded\n"),
        "the group stopped holding its exited member before the TERM was recorded, so this run \
         cannot tell an ended group from a vanished one: {report:#?}"
    );
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
    let Run {
        result,
        report,
        hold_record,
    } = run_under_wall_limit(&[FIXTURE, "term-held-zombie", "1", "5000"]);

    assert_eq!(
        hold_record.as_deref(),
        Some("released after signal 9 was recorded\n"),
        "the group stopped holding its exited member before the KILL was recorded, so this run \
         cannot tell an ended group from a vanished one: {report:#?}"
    );
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
    // Escalation control: catches a change that stops the TERM-then-KILL escalation of a live
    // group whose members ignore TERM, or records either delivered signal as anything else. A live
    // group answers both signals with delivery, never with EPERM, so this run does not reach the
    // refusal branch; a refusal from a live member staying a failure is pinned by the unit test
    // `a_refused_group_signal_with_a_live_member_stays_a_failure` in mlx-guard-core.
    let Run { result, report, .. } =
        run_under_wall_limit(&[FIXTURE, "fanout-ignore-term", "2", "5000"]);

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
