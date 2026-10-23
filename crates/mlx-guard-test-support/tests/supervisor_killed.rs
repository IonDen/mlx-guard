#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

//! A supervisor killed with SIGKILL leaves its owned process group running.
//!
//! This test pins a known limitation, not a guarantee. `mlx-guard run` starts the command in its
//! own process group and cleans that group up from code that runs inside the supervisor (drop,
//! signal forwarding, the parent-exit watch). SIGKILL cannot be caught, so none of that code runs:
//! the command survives, is reparented to `launchd` (PPID 1) and keeps its own process group;
//! the directory holds only the hidden `.<report>.journal`; no final report is ever written.
//!
//! The bug that turns this red: a change that makes the command die with its supervisor (a helper
//! or kernel-side watch that kills or reaps the owned group when the supervisor dies, or launching
//! the command inside the supervisor's own group). Such a change fixes the limitation, so this test
//! and the contract docs that describe it (`PROCESS_CONTROL.md`, `THREAT_MODEL.md`, the README
//! limits) must change together.
//!
//! The command is the bounded `idle-stall` fixture (1 byte, 10 s wall ceiling, no CPU). The orphan
//! is killed by a process-group drop guard, so a failing assertion leaves nothing behind.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_core::{JournalEntry, JournalRecovery};

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// Path to the `mlx-guard` CLI binary, a sibling of this crate's fixture binary in the cargo
/// target directory. `CARGO_BIN_EXE_mlx-guard` exists only inside `mlx-guard-cli`, so the binary
/// must be built first (`cargo test --workspace` and CI's `--all-targets` build it).
fn mlx_guard_binary() -> PathBuf {
    let path = Path::new(FIXTURE)
        .parent()
        .expect("fixture binary has a parent directory")
        .join("mlx-guard");
    assert!(
        path.exists(),
        "the mlx-guard CLI binary is not built at {}; build it with \
         `cargo build -p mlx-guard-cli --bin mlx-guard` first",
        path.display()
    );
    path
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-supervisor-killed-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        // The CLI requires the report's parent directory to be user-owned and 0700.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Kills and reaps the supervisor if the test ends before it was killed on purpose.
struct SupervisorGuard(Child);

impl Drop for SupervisorGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// SIGKILLs the whole owned process group when dropped, so the orphan never outlives the test.
struct OrphanGroupGuard {
    process_group: i32,
}

impl Drop for OrphanGroupGuard {
    fn drop(&mut self) {
        // SAFETY: a negative pid addresses exactly the process group the supervisor created for
        // the fixture; an already-empty group only yields ESRCH.
        unsafe {
            libc::kill(-self.process_group, libc::SIGKILL);
        }
    }
}

/// Report whether the pid still names a live process.
fn process_exists(pid: i32) -> bool {
    // SAFETY: signal zero only probes the pid's liveness and delivers nothing.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

/// Wait until the pid is gone, bounded well below the fixture's own 10 s wall ceiling so the
/// guard, not the fixture's watchdog, has to be what removed it.
fn wait_until_gone(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_exists(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!process_exists(pid), "process {pid} survived the cleanup");
}

/// Read `ppid` and `pgid` of a live process from `ps`.
fn parent_and_group(pid: i32) -> (i32, i32) {
    let output = Command::new("/bin/ps")
        .args(["-o", "ppid=,pgid=", "-p", &pid.to_string()])
        .output()
        .expect("ps must run");
    let listing = String::from_utf8_lossy(&output.stdout);
    let fields = listing
        .split_whitespace()
        .map(|field| field.parse::<i32>().expect("ps must print numbers"))
        .collect::<Vec<_>>();
    let [parent, group] = fields[..] else {
        panic!("expected `ppid pgid` for {pid}, got {listing:?}");
    };
    (parent, group)
}

/// Read the supervisor's single direct child — the command it launched — from `pgrep -P`.
fn only_child_of(parent: i32) -> i32 {
    let output = Command::new("/usr/bin/pgrep")
        .args(["-P", &parent.to_string()])
        .output()
        .expect("pgrep must run");
    let listing = String::from_utf8_lossy(&output.stdout);
    let pids = listing
        .lines()
        .map(|line| line.trim().parse::<i32>().expect("pgrep must print pids"))
        .collect::<Vec<_>>();
    let [pid] = pids[..] else {
        panic!("the supervisor must own exactly one child, got {pids:?}");
    };
    pid
}

/// Poll the journal until its first `Sample` record exists: the supervisor has then observed the
/// command at least once, so killing it is not a race against startup.
fn wait_for_first_sample(journal_path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(recovery) = JournalRecovery::read(journal_path)
            && recovery
                .records
                .iter()
                .any(|record| matches!(record.entry, JournalEntry::Sample(_)))
        {
            return;
        }
        assert!(Instant::now() < deadline, "first sample timed out");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Names of every entry in the directory, sorted.
fn directory_entries(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn sigkilled_supervisor_orphans_its_group_and_leaves_only_the_journal() {
    // Red if the supervisor's death takes the command with it (group reaped or killed on
    // supervisor death, or the command no longer in a group of its own), or if a final report
    // ever appears for a run whose supervisor was SIGKILLed.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let journal_path = directory.0.join(".report.json.journal");
    let supervisor = Command::new(mlx_guard_binary())
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args(["--", FIXTURE, "idle-stall", "1", "10000"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the supervisor must run");
    let supervisor_pid = i32::try_from(supervisor.id()).unwrap();
    let mut supervisor = SupervisorGuard(supervisor);
    wait_for_first_sample(&journal_path);
    let command_pid = only_child_of(supervisor_pid);
    // The owned group is led by the command itself; arm the cleanup before anything can fail.
    let orphan_group = OrphanGroupGuard {
        process_group: command_pid,
    };

    // SAFETY: the pid is this test's own live, unreaped child.
    assert_eq!(
        unsafe { libc::kill(supervisor_pid, libc::SIGKILL) },
        0,
        "SIGKILL to the supervisor must be delivered"
    );
    let status = supervisor
        .0
        .wait()
        .expect("the supervisor must be reapable");
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "the supervisor must have died by SIGKILL: {status:?}"
    );

    // The command outlives its supervisor, held for half a second so a delayed group kill
    // (a watcher that fires shortly after the supervisor dies) would still be caught.
    let observed_until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < observed_until {
        assert!(
            process_exists(command_pid),
            "the command {command_pid} died with its SIGKILLed supervisor"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let (parent, group) = parent_and_group(command_pid);
    assert_eq!(parent, 1, "the orphan must be reparented to launchd");
    assert_eq!(
        group, command_pid,
        "the orphan must still lead its own process group"
    );
    // SAFETY: getpgid(0) only reads this test process's own group id.
    let test_group = unsafe { libc::getpgid(0) };
    assert_ne!(
        group, test_group,
        "the orphan must not share the launcher's group"
    );

    // Only the journal is left; no final report and no temporary report file.
    assert!(!report_path.exists(), "a final report was written");
    assert_eq!(
        directory_entries(&directory.0),
        [".report.json.journal"],
        "only the journal must remain next to the report path"
    );

    // Cleanup: the guard removes the orphan, and the orphan is then gone.
    drop(orphan_group);
    wait_until_gone(command_pid);
}
