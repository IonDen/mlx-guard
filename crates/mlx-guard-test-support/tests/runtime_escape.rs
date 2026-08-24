#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_cli::{RuntimeResult, execute, parse_cli};
use mlx_guard_core::{
    Observed, PolicyState, ReportV1, SignalReason, SignalTarget, SupervisorOutcome, TerminalKind,
};

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");
/// Members the flood scenario spawns: past the 64-identity evidence cap, so the count cannot be
/// coming from the retained evidence list.
const FLOOD_MEMBERS: u64 = 70;
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
            "mlx-guard-escape-{}-{timestamp}-{sequence}",
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

/// Report whether the pid still names a process at all.
///
/// A zombie answers `kill(pid, 0)` with success, so this stays true from the moment an escapee
/// dies until its new parent reaps it. Only `ESRCH` settles the question.
fn process_exists(pid: i32) -> bool {
    // SAFETY: signal zero only probes the fixture pid's liveness and delivers nothing.
    unsafe {
        libc::kill(pid, 0) == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

/// Wait until the pid names nothing, bounded by a deadline rather than a fixed sleep.
///
/// An escapee this test kills is an orphan, so `launchd` reaps its zombie; the wait therefore
/// polls for the reaped state instead of stopping at the kill.
fn wait_until_gone(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    !process_exists(pid)
}

/// Kill and reap the escapees a scenario created, including when an assertion panics first.
///
/// The owned process group cannot reach a process that left it, so this test is the only owner
/// these processes have left.
struct EscapeeCleanup(Vec<i32>);

impl Drop for EscapeeCleanup {
    fn drop(&mut self) {
        for pid in &self.0 {
            // SAFETY: the pid names an escapee this test's own fixture published.
            unsafe { libc::kill(*pid, libc::SIGKILL) };
        }
        for pid in &self.0 {
            // A failed wait must not panic here: unwinding out of a drop during another panic
            // aborts the whole test binary and hides the real assertion.
            let gone = wait_until_gone(*pid);
            assert!(
                gone || thread::panicking(),
                "escapee {pid} outlived cleanup"
            );
        }
    }
}

/// Read the escapee PIDs the fixture published for a test that must clean them up itself.
///
/// A scenario that never publishes the file left nothing outside the owned group, so an absent
/// file reads as no escapees rather than as a failure; each test asserts the count it expects.
fn published_pids(path: &Path) -> Vec<i32> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!(
            "escapee pids at {} were unreadable: {error}",
            path.display()
        ),
    };
    contents
        .lines()
        .map(|line| line.trim().parse().unwrap())
        .collect()
}

/// Supervise one escaping fixture command under a footprint limit no fixture can reach.
///
/// Returns the runtime lock, the process result, the final report, and every escapee PID the
/// fixture published. The caller holds the lock until its own escapee cleanup is done, so a
/// second run never samples while another scenario's escapees are still being reaped.
fn run_escape_scenario(
    wall_time: &str,
    command: &[&str],
) -> (MutexGuard<'static, ()>, RuntimeResult, ReportV1, Vec<i32>) {
    let runtime_lock = RUNTIME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let pid_path = directory.0.join("escapees.pid");
    let pid_file_assignment = format!("MLX_GUARD_FIXTURE_PID_FILE={}", pid_path.display());
    let mut arguments = vec![
        "mlx-guard",
        "run",
        "--max-footprint",
        "1TiB",
        "--wall-time",
        wall_time,
        "--sample-interval",
        "10ms",
        "--report",
        report_path.to_str().unwrap(),
        "--env",
        pid_file_assignment.as_str(),
        "--",
    ];
    arguments.extend_from_slice(command);
    let parsed = parse_cli(arguments).unwrap();

    let result = execute(parsed);

    let report = ReportV1::from_json(&fs::read_to_string(&report_path).unwrap()).unwrap();
    let escapees = published_pids(&pid_path);
    (runtime_lock, result, report, escapees)
}

#[test]
fn a_setsid_escape_is_counted_once_and_an_empty_group_is_not_containment() {
    // Catches counting one escapee once per sample, and catches presenting an emptied owned group
    // as containment while a process this run started keeps running outside it.
    let (_runtime_lock, result, report, escapees) =
        run_escape_scenario("300ms", &[FIXTURE, "setsid-parent", "1", "800"]);
    let _cleanup = EscapeeCleanup(escapees.clone());

    assert_eq!(escapees.len(), 1, "the fixture published {escapees:?}");
    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
    assert_eq!(
        report.escape.detected,
        Observed::Available { value: true },
        "{report:#?}"
    );
    assert_eq!(report.escape.escaped_count, Some(1), "{report:#?}");
    // Every signal this run sent aimed at the owned group; no signal ever aimed at the escapee,
    // which is exactly why the group going quiet is not containment.
    assert!(
        report
            .signals
            .iter()
            .all(|signal| signal.target == SignalTarget::OwnedProcessGroup),
        "{report:#?}"
    );
    assert!(
        process_exists(escapees[0]),
        "the escapee must still be running after the owned group is gone"
    );
}

#[test]
fn a_daemonized_grandchild_is_a_counted_escape() {
    // Catches losing a daemonized descendant once its intermediate exits, and catches counting
    // that exited intermediate as a second escape.
    //
    // The wall is what ends this run, not what it measures: daemonizing costs three sequential
    // process launches plus the fixture's observation hold, so the run gets room for all of them
    // on a machine whose scheduler is under pressure.
    let (_runtime_lock, result, report, escapees) =
        run_escape_scenario("800ms", &[FIXTURE, "daemonize", "1", "2000"]);
    let _cleanup = EscapeeCleanup(escapees.clone());

    assert_eq!(escapees.len(), 1, "the fixture published {escapees:?}");
    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert_eq!(
        report.escape.detected,
        Observed::Available { value: true },
        "{report:#?}"
    );
    assert_eq!(report.escape.escaped_count, Some(1), "{report:#?}");
    assert!(
        process_exists(escapees[0]),
        "the daemon must still be running after the owned group is gone"
    );
}

#[test]
fn a_plain_double_fork_reparents_without_escaping() {
    // Catches reading a reparented descendant as an escape: it left its parent, not the owned
    // process group, so the group's own termination still reaches it.
    let (_runtime_lock, result, report, _escapees) =
        run_escape_scenario("300ms", &[FIXTURE, "double-fork", "1", "800"]);

    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert_eq!(
        report.escape.detected,
        Observed::Available { value: false },
        "{report:#?}"
    );
    assert_eq!(report.escape.escaped_count, None, "{report:#?}");
    // The reparented grandchild kept the owned PGID, so the group signal reached it and this test
    // has nothing of its own to clean up.
    assert!(
        report.signals.iter().any(|signal| {
            signal.signal == 15
                && signal.target == SignalTarget::OwnedProcessGroup
                && signal.reason == Some(SignalReason::WallTime)
        }),
        "{report:#?}"
    );
}

#[test]
fn a_flood_past_the_evidence_cap_counts_every_escape_exactly() {
    // Catches a count bounded by the 64-identity evidence cap, and catches a count that grows per
    // sample instead of per distinct escaped identity.
    let members = FLOOD_MEMBERS.to_string();
    let (_runtime_lock, result, report, escapees) = run_escape_scenario(
        "2500ms",
        &[FIXTURE, "escape-flood", members.as_str(), "3000"],
    );
    let _cleanup = EscapeeCleanup(escapees.clone());

    assert_eq!(
        u64::try_from(escapees.len()).unwrap(),
        FLOOD_MEMBERS,
        "the fixture published {escapees:?}"
    );
    assert_eq!(
        report.escape.detected,
        Observed::Available { value: true },
        "{report:#?}"
    );
    assert_eq!(
        report.escape.escaped_count,
        Some(FLOOD_MEMBERS),
        "{report:#?}"
    );
    // A flood must not be read as measurement loss: the escapees are observed, not missing.
    assert!(
        report
            .transitions
            .iter()
            .all(|transition| transition.to != PolicyState::SupervisorError),
        "{report:#?}"
    );
    assert_eq!(result.outcome, SupervisorOutcome::PolicyIntervention);
    assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
    assert!(
        report.signals.iter().any(|signal| {
            signal.signal == 15
                && signal.target == SignalTarget::OwnedProcessGroup
                && signal.reason == Some(SignalReason::WallTime)
        }),
        "{report:#?}"
    );
}
