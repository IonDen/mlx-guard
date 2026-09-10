#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

//! A command that retires children quickly must not read as observation failure. Before the fix
//! this pins, every tracked child's exit between two samples counted as one unusable sample, so a
//! shell loop of 50 ms tools at the default 50 ms interval ended `observe` with exit 70 after five
//! samples. The real binary is driven here because the streak logic lives in the runtime.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_core::{ReportV1, TerminalKind};

const SUPERVISOR: &str = env!("CARGO_BIN_EXE_mlx-guard");

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-child-churn-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Best-effort cleanup of the `/bin/sh` loop a relinquishing supervisor leaves behind: it is no
/// longer in any group this test owns, so match it by its command line.
fn end_relinquished_churn() {
    let _ = Command::new("/usr/bin/pkill")
        .args(["-f", "while :; do /bin/sleep 0.05; done"])
        .status();
}

#[test]
fn observe_survives_a_loop_of_short_lived_children_at_the_default_interval() {
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let mut guard = Command::new(SUPERVISOR)
        .args(["observe", "--sample-interval", "50ms", "--report"])
        .arg(&report_path)
        .args(["--", "/bin/sh", "-c", "while :; do /bin/sleep 0.05; done"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // Not piped: the workload inherits the supervisor's stderr, and a supervisor that gives up
        // relinquishes the churn loop, which would then hold a pipe open for good.
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Sixty samples' worth of churn: the old behaviour gave up inside the first five.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(status) = guard.try_wait().unwrap() {
            // A relinquished loop would outlive the test; end it before failing.
            end_relinquished_churn();
            panic!("the supervisor gave up on a healthy churning command: {status:?}");
        }
        thread::sleep(Duration::from_millis(50));
    }

    let guard_pid = i32::try_from(guard.id()).unwrap();
    // SAFETY: the supervisor is this test's own unreaped child.
    unsafe {
        libc::kill(guard_pid, libc::SIGTERM);
    }
    let status = guard.wait().unwrap();
    assert_eq!(status.code(), Some(128 + libc::SIGTERM), "{status:?}");

    let report = ReportV1::from_json(&fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(
        report.outcome.kind,
        TerminalKind::ChildSignaled {
            signal: u8::try_from(libc::SIGTERM).unwrap()
        }
    );
    assert!(
        report.samples.len() >= 40,
        "too few samples to have crossed the churn: {}",
        report.samples.len()
    );
}
