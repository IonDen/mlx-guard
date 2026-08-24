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
#[allow(dead_code)] // consumed once Task 6 wires `spawn_via_launcher` into real e2e tests
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
#[allow(dead_code)] // consumed once Task 6 appends its e2e tests
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
#[allow(dead_code)] // consumed once Task 6 appends its e2e tests
fn wait_until_gone(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!process_exists(pid));
}

/// Poll the run's journal until its first `Sample` record exists — the gate that makes killing
/// the launcher race-free (the supervisor must have observed the child at least once first).
#[allow(dead_code)] // consumed once Task 6 appends its e2e tests
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
