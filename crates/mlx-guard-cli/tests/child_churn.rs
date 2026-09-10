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

/// The churn loop, tagged with this test process's pid so cleanup and the post-exit check match
/// only this run's loop, never a sibling checkout's.
fn churn_program() -> String {
    format!(
        "while :; do /bin/sleep 0.05; done # mlx-guard-child-churn-{}",
        std::process::id()
    )
}

/// Anchored: the tag ends the `sh -c` string, while the report directory (which also carries the
/// prefix) never ends with it, and a longer pid must not match a shorter one's prefix.
fn churn_tag_pattern() -> String {
    format!("mlx-guard-child-churn-{}$", std::process::id())
}

fn churn_loop_alive() -> bool {
    Command::new("/usr/bin/pgrep")
        .args(["-f", &churn_tag_pattern()])
        .stdout(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Best-effort cleanup of the loop a relinquishing supervisor leaves behind: it is no longer in
/// any group this test owns, so match it by its tagged command line.
fn end_relinquished_churn() {
    let _ = Command::new("/usr/bin/pkill")
        .args(["-f", &churn_tag_pattern()])
        .status();
}

/// Runs the cleanup on every exit path, including a panic in any assertion below.
struct ChurnCleanup;

impl Drop for ChurnCleanup {
    fn drop(&mut self) {
        end_relinquished_churn();
    }
}

#[test]
fn observe_survives_a_loop_of_short_lived_children_at_the_default_interval() {
    let directory = TestDirectory::new();
    let _cleanup = ChurnCleanup;
    let report_path = directory.0.join("report.json");
    let mut guard = Command::new(SUPERVISOR)
        .args(["observe", "--sample-interval", "50ms", "--report"])
        .arg(&report_path)
        .args(["--", "/bin/sh", "-c", &churn_program()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // Not piped: the workload inherits the supervisor's stderr, and a supervisor that gives up
        // relinquishes the churn loop, which would then hold a pipe open for good.
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Eighty samples' worth of churn at the nominal rate: the old behaviour gave up inside the
    // first five.
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        if let Some(status) = guard.try_wait().unwrap() {
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
    // The observe calibration counts every sample of the run. Under churn every sample must have
    // been complete (the falsifier), and enough of them must exist to have crossed the old
    // failure point (five samples) three times over; the starved CI runner managed 34 in 3 s, so
    // the floor is not a wall-clock rate it can miss.
    let calibration = report
        .calibration
        .expect("observe reports carry calibration");
    assert_eq!(
        calibration.incomplete_samples, 0,
        "a child's exit still made a sample incomplete: {calibration:?}"
    );
    assert!(
        calibration.complete_samples >= 15,
        "too few complete samples to have crossed the churn: {calibration:?}"
    );
    // The supervisor forwards the terminal signal to its owned group; the loop must be gone.
    let cleanup_deadline = Instant::now() + Duration::from_secs(2);
    while churn_loop_alive() && Instant::now() < cleanup_deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !churn_loop_alive(),
        "the churn loop outlived the supervisor's terminal-signal forwarding"
    );
}
