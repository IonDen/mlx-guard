#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

//! Three-generation parent-death e2e harness: test → `/bin/sh` launcher script → supervisor →
//! worker. Killing the launcher (this test's own child) reparents the supervisor to `launchd`
//! exactly like a real shell that started `mlx-guard` and then died itself.
//!
//! This crate is the only place `CARGO_BIN_EXE_mlx-guard` is visible (review C1): `mlx-guard` is
//! `mlx-guard-cli`'s own binary; `mlx-guard-test-support`'s fixture binaries are not, and are not
//! needed here — workers are plain `/bin/sleep` and `/bin/sh` (see `shell.rs:417,463`).
//!
//! Real `Command` processes, not in-process `execute()`, drive every test: the parent that dies
//! must be a real OS process. Unlike `runtime_awkward_exits.rs` / `runtime_checkpoint.rs`, no
//! `RUNTIME_LOCK` guards these tests — each spawns its own subprocess tree, so there is no
//! process-wide state (installed signal handlers, etc.) for parallel tests to contend over, the
//! same reasoning `shell.rs` already relies on for its own real-binary tests.

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// `mlx-guard`'s own binary, built fresh for the test run.
const SUPERVISOR: &str = env!("CARGO_BIN_EXE_mlx-guard");

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-parent-death-{}-{timestamp}-{sequence}",
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

/// Write the three-generation launcher script into `directory` and return its path.
///
/// The script backgrounds its argument command, reports the backgrounded pid, then waits on it —
/// so killing the script (this test's own child) leaves the backgrounded command running as an
/// orphan under `launchd`, exactly like a shell that launched `mlx-guard` and was itself killed.
/// `ignore_hangup` models `nohup` (`trap '' HUP` is inherited across `exec` as `SIG_IGN`).
fn write_launcher_script(directory: &Path, ignore_hangup: bool) -> PathBuf {
    let body = if ignore_hangup {
        "#!/bin/sh\ntrap '' HUP\n\"$@\" &\necho \"LAUNCHED $!\"\nwait\n"
    } else {
        "#!/bin/sh\n\"$@\" &\necho \"LAUNCHED $!\"\nwait\n"
    };
    let path = directory.join("launcher.sh");
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// Spawn `/bin/sh <script> <command> <args...>`, piping the launcher's stdout, and return the
/// launcher `Child` plus the pid parsed from its first `LAUNCHED <pid>` line.
fn spawn_launcher(script: &Path, command: &str, args: &[&str]) -> (Child, i32) {
    let mut launcher = Command::new("/bin/sh")
        .arg(script)
        .arg(command)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .expect("the launcher must run");
    let mut reader = BufReader::new(
        launcher
            .stdout
            .take()
            .expect("the launcher's stdout must be piped"),
    );
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("the launcher must report the backgrounded pid");
    let pid: i32 = line
        .trim()
        .strip_prefix("LAUNCHED ")
        .unwrap_or_else(|| panic!("expected a LAUNCHED <pid> line, got {line:?}"))
        .parse()
        .unwrap_or_else(|error| panic!("backgrounded pid must parse: {error}"));
    (launcher, pid)
}

/// Spawn launcher → supervisor(args) and return (launcher `Child`, supervisor pid) after parsing
/// the `LAUNCHED` line from the launcher's piped stdout.
fn spawn_via_launcher(script: &Path, supervisor_args: &[&str]) -> (Child, i32) {
    spawn_launcher(script, SUPERVISOR, supervisor_args)
}

/// Report whether the pid still names a process this test is allowed to signal.
fn process_exists(pid: i32) -> bool {
    // SAFETY: signal zero only probes the pid's liveness and delivers nothing.
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

/// Poll the run's journal until its first `Sample` record exists — the gate that makes killing
/// the launcher race-free (the supervisor must have observed the child at least once first).
fn wait_for_first_sample(journal_path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(recovery) = mlx_guard_core::JournalRecovery::read(journal_path)
            && recovery
                .records
                .iter()
                .any(|record| matches!(record.entry, mlx_guard_core::JournalEntry::Sample(_)))
        {
            return;
        }
        assert!(Instant::now() < deadline, "first sample timed out");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Wait for the run's final report to appear, then parse and validate it.
///
/// The report is replaced atomically, so a file that exists is already complete: a parse or schema
/// failure is a real defect and panics here instead of expiring the deadline.
fn read_report_within(report_path: &Path, timeout: Duration) -> mlx_guard_core::ReportV1 {
    let deadline = Instant::now() + timeout;
    while !report_path.exists() {
        assert!(
            Instant::now() < deadline,
            "final report timed out: {}",
            report_path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
    mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(report_path).expect("final report must be readable"),
    )
    .expect("final report must be schema-valid")
}

/// Kill and reap the launcher this test owns, orphaning the supervisor onto `launchd`.
fn kill_and_reap(launcher: &mut Child) {
    launcher.kill().expect("the launcher must be killable");
    launcher.wait().expect("the launcher must be reapable");
}

#[test]
fn write_launcher_script_reports_the_backgrounded_pid_and_reaps_it() {
    // Catches a launcher script that never echoes LAUNCHED <pid>, never backgrounds its argument
    // command, or leaves its own shell process alive after the backgrounded command finishes.
    let directory = TestDirectory::new();
    let script = write_launcher_script(&directory.0, false);

    let (mut launcher, pid) = spawn_launcher(&script, "/bin/sleep", &["0.1"]);
    assert!(
        process_exists(pid),
        "the backgrounded sleep must still be alive right after LAUNCHED"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = launcher
            .try_wait()
            .expect("polling the launcher must not fail")
        {
            break status;
        }
        assert!(Instant::now() < deadline, "launcher exit timed out");
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "launcher must exit 0: {status:?}");
}

#[test]
fn killing_the_parent_terminates_the_group_and_reports_parent_exit() {
    // Catches a supervisor that keeps enforcing for a parent that no longer exists: an orphaned
    // run must terminate the owned group and name the parent's exit as the reason it did.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let journal_path = directory.0.join(".report.json.journal");
    let script = write_launcher_script(&directory.0, false);
    let (mut launcher, supervisor) = spawn_via_launcher(
        &script,
        &[
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
            "--report",
            report_path.to_str().unwrap(),
            "--",
            "/bin/sleep",
            "30",
        ],
    );
    // Killing the launcher only after the first sample keeps the race out of the test: the
    // supervisor has provably observed its worker at least once by then.
    wait_for_first_sample(&journal_path);

    kill_and_reap(&mut launcher);

    let report = read_report_within(&report_path, Duration::from_secs(5));
    wait_until_gone(supervisor);
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::PolicyIntervention
    );
    // `sleep` obeys TERM, so the shutdown never escalates to KILL.
    assert_eq!(
        report
            .signals
            .iter()
            .map(|signal| (signal.signal, signal.reason, signal.result))
            .collect::<Vec<_>>(),
        [(
            15,
            Some(mlx_guard_core::SignalReason::ParentExit),
            mlx_guard_core::SignalResult::Delivered,
        )],
        "{report:#?}"
    );
    assert_eq!(
        report.outcome.child_status,
        Some(mlx_guard_core::ChildStatus::Signaled { signal: 15 })
    );
    // The schema validator already enforces that this time precedes the outcome it explains.
    assert!(report.outcome.parent_exited_at_ms.is_some(), "{report:#?}");
    assert_eq!(
        report.configuration.parent_watch,
        Some(mlx_guard_core::ParentWatch::Active)
    );
    assert_eq!(
        report.configuration.on_parent_exit,
        Some(mlx_guard_core::OnParentExit::Terminate)
    );
    // The launcher that would have reaped the supervisor is gone, so no exit code is observable
    // here. A `policy_intervention` outcome IS exit 75 by the independently pinned mapping
    // (`mlx-guard-core/tests/exit_semantics.rs:9` and the 75 row of `_expected_exit_code` in
    // `python/mlx_guard/_client.py`); the hangup-ignored test below observes a live exit code.
}

#[test]
fn detach_leaves_the_run_alone_and_records_the_orphan_time() {
    // Catches `--on-parent-exit=detach` terminating the run anyway, and a detached run losing the
    // evidence that it was orphaned at all.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let journal_path = directory.0.join(".report.json.journal");
    let script = write_launcher_script(&directory.0, false);
    let (mut launcher, supervisor) = spawn_via_launcher(
        &script,
        &[
            "run",
            "--on-parent-exit",
            "detach",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
            "--report",
            report_path.to_str().unwrap(),
            "--",
            "/bin/sleep",
            "2",
        ],
    );
    wait_for_first_sample(&journal_path);

    kill_and_reap(&mut launcher);

    // Positive control: the orphaned supervisor keeps supervising instead of shutting down.
    thread::sleep(Duration::from_secs(1));
    assert!(
        process_exists(supervisor),
        "a detached supervisor must outlive the parent that launched it"
    );
    let report = read_report_within(&report_path, Duration::from_secs(10));
    wait_until_gone(supervisor);
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::ChildExited { code: 0 }
    );
    assert!(report.signals.is_empty(), "{report:#?}");
    assert!(report.outcome.parent_exited_at_ms.is_some(), "{report:#?}");
    assert_eq!(
        report.configuration.parent_watch,
        Some(mlx_guard_core::ParentWatch::Detach)
    );
    assert_eq!(
        report.configuration.on_parent_exit,
        Some(mlx_guard_core::OnParentExit::Detach)
    );
}

#[test]
fn a_sighup_to_the_supervisor_forwards_on_the_terminal_path() {
    // Catches SIGHUP killing the supervisor outright instead of being captured, forwarded to the
    // owned group, and recorded as the external signal it is.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let journal_path = directory.0.join(".report.json.journal");
    let guard = Command::new(SUPERVISOR)
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args(["--", "/bin/sleep", "30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the supervisor must run");
    wait_for_first_sample(&journal_path);

    let delivered = Command::new("/bin/kill")
        .args(["-HUP", &guard.id().to_string()])
        .status()
        .expect("SIGHUP delivery command must run");
    assert!(delivered.success());
    let output = guard.wait_with_output().expect("supervisor must terminate");

    assert_eq!(output.status.code(), Some(129));
    let report = read_report_within(&report_path, Duration::from_secs(5));
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::ChildSignaled { signal: 1 }
    );
    assert!(
        report.signals.iter().any(|signal| signal.signal == 1
            && signal.reason == Some(mlx_guard_core::SignalReason::ExternalSignal)
            && signal.result == mlx_guard_core::SignalResult::Delivered),
        "{report:#?}"
    );
}

#[test]
fn a_dead_stdout_reader_does_not_turn_the_result_into_a_panic() {
    // Catches a finalized run becoming exit 101 because whoever held our stdout went away. The
    // worker writes nothing on purpose: it inherits the same closed pipe, and `Command` restores
    // the default SIGPIPE disposition in children, so a writing worker would die 141 of its own.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let mut guard = Command::new(SUPERVISOR)
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args(["--", "/usr/bin/true"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("the supervisor must run");
    drop(
        guard
            .stdout
            .take()
            .expect("the supervisor's stdout must be piped"),
    );

    let status = guard.wait().expect("supervisor must terminate");

    assert_eq!(
        status.code(),
        Some(0),
        "a closed summary pipe must not turn a finished run into a panic"
    );
}

#[test]
fn a_launcher_that_ignored_sighup_disables_the_watch_and_says_so() {
    // Catches a `nohup`-style launcher (SIGHUP already SIG_IGN) still arming the watch: the report
    // must say the watch was off, and the run must end on its own wall-time limit instead.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let journal_path = directory.0.join(".report.json.journal");
    let script = write_launcher_script(&directory.0, true);
    let (mut launcher, supervisor) = spawn_via_launcher(
        &script,
        &[
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
            "/bin/sleep",
            "30",
        ],
    );
    wait_for_first_sample(&journal_path);

    kill_and_reap(&mut launcher);

    let report = read_report_within(&report_path, Duration::from_secs(10));
    wait_until_gone(supervisor);
    assert_eq!(
        report.configuration.parent_watch,
        Some(mlx_guard_core::ParentWatch::HangupIgnored)
    );
    assert_eq!(report.outcome.parent_exited_at_ms, None, "{report:#?}");
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::PolicyIntervention
    );
    assert_eq!(
        report
            .signals
            .iter()
            .map(|signal| (signal.signal, signal.reason))
            .collect::<Vec<_>>(),
        [(15, Some(mlx_guard_core::SignalReason::WallTime))],
        "{report:#?}"
    );
}
