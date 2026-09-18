//! A terminal on the supervisor's standard input no longer refuses the launch: the command reads
//! `/dev/null` instead and the supervisor says so on one stderr line. Every other stdin is inherited
//! silently.

#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::fs::{self, File};
use std::io::IsTerminal;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TERMINAL_LINE: &str =
    "mlx-guard: standard input is a terminal, so the command reads from /dev/null instead";

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
            "mlx-guard-tty-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        // Tolerant on purpose: on the regression path the supervisor may still be finalising its
        // report here, and a second panic during unwinding would abort the whole test binary.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A fresh pseudo-terminal pair. The master stays open for the test's lifetime so the slave never
/// reports hang-up; nothing is ever written to it, so a command that reads the slave blocks.
struct Pty {
    _master: File,
    slave: Option<File>,
}

impl Pty {
    fn open() -> Self {
        let mut descriptors = [0; 2];
        // SAFETY: openpty initializes both descriptors; no name or attributes are requested.
        assert_eq!(
            unsafe {
                libc::openpty(
                    descriptors.as_mut_ptr(),
                    descriptors.as_mut_ptr().add(1),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        // SAFETY: openpty returned two owned descriptors exactly once.
        let (master, slave) = unsafe {
            (
                File::from_raw_fd(descriptors[0]),
                File::from_raw_fd(descriptors[1]),
            )
        };
        assert!(slave.is_terminal());
        Self {
            _master: master,
            slave: Some(slave),
        }
    }

    fn stdin(&mut self) -> Stdio {
        Stdio::from(self.slave.take().expect("the slave is handed out once"))
    }
}

/// Wait for the supervisor with a bound: a command that inherited the pseudo-terminal blocks on it
/// forever, and this turns that regression into a failed assertion instead of a hung test.
fn wait_bounded(mut child: Child, deadline: Duration) -> (ExitStatus, Vec<u8>, Vec<u8>) {
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            return (output.status, output.stdout, output.stderr);
        }
        if started.elapsed() >= deadline {
            // Leave no orphaned supervisor or command behind on the failure path.
            child.kill().ok();
            child.wait().ok();
            panic!(
                "the supervisor did not finish within {deadline:?}: the command is blocked on stdin"
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn guard() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
}

/// The command's stdout, with the supervisor's own summary line (always last) stripped off.
fn command_stdout(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout).into_owned();
    let (before, summary) = text
        .rsplit_once("mlx-guard: child_exited")
        .expect("the summary line must close stdout");
    assert!(summary.ends_with('\n'), "{text:?}");
    before.to_owned()
}

fn report_kind(path: &PathBuf) -> mlx_guard_core::TerminalKind {
    mlx_guard_core::ReportV1::from_json(&fs::read_to_string(path).expect("final report must exist"))
        .unwrap()
        .outcome
        .kind
}

#[test]
fn run_with_a_terminal_on_stdin_gives_the_command_dev_null_and_says_so() {
    // Red if the terminal is refused (exit 64, nothing launched), if the terminal is passed on
    // (cat blocks, the wall time ends it with 75), or if the diagnostic line is missing or worded
    // differently from the documented one.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let mut pty = Pty::open();
    let child = guard()
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "5s",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args(["--", "/bin/cat"])
        .stdin(pty.stdin())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the supervisor must start");
    let (status, stdout, stderr) = wait_bounded(child, Duration::from_secs(10));

    assert_eq!(
        status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        command_stdout(&stdout),
        "",
        "cat must read end-of-file, not the terminal"
    );
    let text = String::from_utf8_lossy(&stderr);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "one terminal line plus the launch banner: {text:?}"
    );
    assert_eq!(lines[0], TERMINAL_LINE);
    assert!(lines[1].starts_with("mlx-guard: enforcing a "), "{text:?}");
    assert_eq!(
        report_kind(&report_path),
        mlx_guard_core::TerminalKind::ChildExited { code: 0 }
    );
}

#[test]
fn observe_with_a_terminal_on_stdin_gives_the_command_dev_null_and_says_so() {
    // Red if the observe path skips the substitution or the diagnostic (observe has no banner, so
    // the line must be the only stderr output).
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let mut pty = Pty::open();
    let child = guard()
        .args(["observe", "--sample-interval", "10ms", "--report"])
        .arg(&report_path)
        .args(["--", "/bin/cat"])
        .stdin(pty.stdin())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the supervisor must start");
    let (status, stdout, stderr) = wait_bounded(child, Duration::from_secs(10));

    assert_eq!(
        status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(command_stdout(&stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&stderr),
        format!("{TERMINAL_LINE}\n")
    );
    assert_eq!(
        report_kind(&report_path),
        mlx_guard_core::TerminalKind::ChildExited { code: 0 }
    );
}

#[test]
fn a_command_that_exits_at_once_still_gets_the_terminal_line() {
    // Pins the line for a command that exits at once. It turns red on the old placement (after
    // identity inspection) only when the root is already gone at the first inspect, which is
    // timing-dependent; the placement itself is pinned by the two call sites in
    // prepare_observe_worker / prepare_run_worker. Red deterministically if the line is dropped.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let mut pty = Pty::open();
    let child = guard()
        .args(["observe", "--sample-interval", "10ms", "--report"])
        .arg(&report_path)
        .args(["--", "/usr/bin/true"])
        .stdin(pty.stdin())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the supervisor must start");
    let (status, _stdout, stderr) = wait_bounded(child, Duration::from_secs(10));

    assert_eq!(
        status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&stderr),
        format!("{TERMINAL_LINE}\n")
    );
}

#[test]
fn a_non_terminal_stdin_is_inherited_without_a_diagnostic() {
    // Red if the line is printed unconditionally, or if /dev/null is substituted for every stdin
    // (the piped bytes would then never reach cat).
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let mut child = guard()
        .args(["observe", "--sample-interval", "10ms", "--report"])
        .arg(&report_path)
        .args(["--", "/bin/cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the supervisor must start");
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"piped bytes\n").unwrap();
    }
    let (status, stdout, stderr) = wait_bounded(child, Duration::from_secs(10));

    assert_eq!(
        status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(command_stdout(&stdout), "piped bytes\n");
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    assert_eq!(
        report_kind(&report_path),
        mlx_guard_core::TerminalKind::ChildExited { code: 0 }
    );
}
