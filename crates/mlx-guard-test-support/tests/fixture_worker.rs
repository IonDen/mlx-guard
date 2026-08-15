#![allow(unsafe_code)]

use std::fs::{self, File};
use std::io::Read;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");

struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Session {
    fn spawn(mode: &str, bytes: u64, wall_ms: u64) -> Self {
        let mut child = Command::new(FIXTURE)
            .args([mode, &bytes.to_string(), &wall_ms.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("fixture must launch");
        let stdin = child.stdin.take().expect("fixture stdin must be piped");
        let stdout = child.stdout.take().expect("fixture stdout must be piped");
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                sender
                    .send(line.expect("fixture output must be UTF-8"))
                    .ok();
            }
        });
        Self {
            child,
            stdin,
            lines,
        }
    }

    fn expect_line(&self, expected: &str) {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(2))
            .expect("fixture phase must be bounded");
        assert_eq!(line, expected);
    }

    fn send(&mut self, command: &str) {
        writeln!(self.stdin, "{command}").expect("fixture command must be writable");
        self.stdin.flush().expect("fixture command must be flushed");
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.child.kill().ok();
        }
        self.child.wait().ok();
    }
}

#[test]
fn rejects_requests_outside_the_hard_caps() {
    // Catches bypassing FixtureLimits in the executable boundary.
    let output = Command::new(FIXTURE)
        .args(["allocate", "134217729", "1"])
        .output()
        .expect("fixture must run");
    assert_eq!(output.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&output.stderr).contains("outside"));

    let output = Command::new(FIXTURE)
        .args(["allocate", "1", "0"])
        .output()
        .expect("fixture must run");
    assert_eq!(output.status.code(), Some(64));
}

#[test]
fn allocate_worker_acknowledges_release_and_exit() {
    // Catches missing or reordered allocate, release, and exit phases.
    let mut session = Session::spawn("allocate", 8 * 1024 * 1024, 1_000);
    session.expect_line("READY mode=allocate bytes=8388608");
    session.send("allocate");
    session.expect_line("ALLOCATED");
    session.send("release");
    session.expect_line("RELEASED");
    session.send("exit");
    session.expect_line("EXIT");
    assert!(session.child.wait().expect("fixture must exit").success());
}

#[test]
fn watchdog_terminates_a_stalled_interactive_fixture() {
    // Catches a blocking stdin read that can outlive the advertised wall ceiling.
    let mut session = Session::spawn("allocate", 1, 50);
    session.expect_line("READY mode=allocate bytes=1");
    let status = session
        .child
        .wait()
        .expect("watchdog must terminate the fixture");
    assert_eq!(status.code(), Some(124));
}

#[test]
fn cpu_stall_worker_remains_bounded() {
    // Catches an unbounded busy loop or a mode that never performs work.
    let output = Command::new(FIXTURE)
        .args(["cpu-stall", "1", "25"])
        .output()
        .expect("fixture must run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("READY mode=cpu-stall bytes=1\n"));
    let iterations: u64 = stdout
        .lines()
        .find_map(|line| line.strip_prefix("DONE iterations="))
        .expect("CPU fixture must report completed work")
        .parse()
        .expect("iteration count must be numeric");
    assert!(iterations > 0);
}

#[test]
fn fanout_worker_exposes_a_bounded_real_process_group() {
    // Catches replacing the 16-member performance fixture with synthetic identity records.
    let output = Command::new(FIXTURE)
        .args(["fanout-stall", "16", "100"])
        .output()
        .expect("fanout fixture must run");
    assert_eq!(output.status.code(), Some(124));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "READY mode=fanout-stall members=16\n"
    );
}

#[test]
fn shared_mapping_worker_acknowledges_unmap() {
    // Catches substituting MAP_PRIVATE or a synthetic acknowledgement for the shared mapping.
    let mut session = Session::spawn("shared", 8 * 1024 * 1024, 1_000);
    session.expect_line("READY mode=shared bytes=8388608");
    session.send("allocate");
    session.expect_line("ALLOCATED");
    session.send("release");
    session.expect_line("RELEASED");
    session.send("exit");
    session.expect_line("EXIT");
    assert!(session.child.wait().expect("fixture must exit").success());
}

#[test]
fn churn_and_fast_root_exit_are_real_process_behaviors() {
    // Catches a churn mode that only prints synthetic PIDs or a root that waits for its child.
    let churn = Command::new(FIXTURE)
        .args(["spawn-churn", "1", "1000"])
        .output()
        .expect("churn fixture must run");
    assert!(churn.status.success());
    assert!(String::from_utf8_lossy(&churn.stdout).contains("CHURNED children=6"));

    let fast = Command::new(FIXTURE)
        .args(["fast-root-exit", "1", "1000"])
        .output()
        .expect("fast-exit fixture must run");
    assert_eq!(fast.status.code(), Some(23));
    let stdout = String::from_utf8_lossy(&fast.stdout);
    let child_pid: i32 = stdout
        .lines()
        .find_map(|line| line.strip_prefix("CHILD pid="))
        .expect("fixture must report its real child")
        .parse()
        .expect("reported child PID must be numeric");
    assert_eq!(unsafe { libc::kill(child_pid, 0) }, 0);
}

#[test]
fn setsid_and_ignore_term_modes_expose_real_unix_state() {
    // Catches a fake escape report or a TERM handler that still terminates normally.
    let escaped = Command::new(FIXTURE)
        .args(["setsid", "1", "100"])
        .output()
        .expect("setsid fixture must run");
    assert!(escaped.status.success());
    let stdout = String::from_utf8_lossy(&escaped.stdout);
    let fields: Vec<_> = stdout
        .lines()
        .find(|line| line.starts_with("ESCAPED "))
        .expect("setsid fixture must report identity")
        .split_whitespace()
        .collect();
    let pid = parse_field(&fields, "pid");
    assert_eq!(parse_field(&fields, "pgid"), pid);
    assert_eq!(parse_field(&fields, "sid"), pid);

    // A 2 s wall budget (matching the other ignore-term fixtures) keeps the liveness check below
    // from racing the watchdog on a slow shared CI runner; the final wait still proves exit 124.
    let mut ignored = Session::spawn("ignore-term", 1, 2_000);
    ignored.expect_line(&format!(
        "READY mode=ignore-term pid={}",
        ignored.child.id()
    ));
    let result = unsafe { libc::kill(ignored.child.id().cast_signed(), libc::SIGTERM) };
    assert_eq!(result, 0);
    thread::sleep(Duration::from_millis(20));
    assert!(ignored.child.try_wait().expect("wait must work").is_none());
    assert_eq!(
        ignored.child.wait().expect("watchdog must finish").code(),
        Some(124)
    );
}

#[test]
fn inherited_fd_frame_is_binary_and_separate_from_stdout() {
    // Catches routing protocol bytes through stdout or text-encoding a raw frame.
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let mut reader = unsafe { File::from_raw_fd(fds[0]) };
    let writer = unsafe { File::from_raw_fd(fds[1]) };
    assert_ne!(
        unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETFD, 0) },
        -1
    );

    let child = Command::new(FIXTURE)
        .args(["raw-fd", "1", "100"])
        .env("MLX_GUARD_FIXTURE_FD", writer.as_raw_fd().to_string())
        .stdout(Stdio::piped())
        .spawn()
        .expect("raw-fd fixture must launch");
    drop(writer);
    let mut frame = Vec::new();
    reader
        .read_to_end(&mut frame)
        .expect("frame must be readable");
    let output = child
        .wait_with_output()
        .expect("raw-fd fixture must finish");
    assert!(output.status.success());
    assert_eq!(frame, b"\0\0\0\x0ffixture-frame\0\xff");
    assert!(
        !output
            .stdout
            .windows(13)
            .any(|value| value == b"fixture-frame")
    );
}

#[test]
fn artifact_failure_is_observed_without_creating_a_file() {
    // Catches swallowing an artifact creation error or falsely acknowledging persistence.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock must follow the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "mlx-guard-artifact-failure-{}-{unique}",
        std::process::id(),
    ));
    fs::create_dir(&path).expect("temporary blocker directory must be created");
    let output = Command::new(FIXTURE)
        .args(["artifact-failure", "1", "100"])
        .env("MLX_GUARD_FIXTURE_PATH", &path)
        .output()
        .expect("artifact fixture must run");
    fs::remove_dir(&path).expect("temporary blocker directory must be removable");
    assert_eq!(output.status.code(), Some(74));
    assert!(String::from_utf8_lossy(&output.stderr).contains("artifact create failed"));
}

fn parse_field(fields: &[&str], name: &str) -> i32 {
    fields
        .iter()
        .find_map(|field| field.strip_prefix(&format!("{name}=")))
        .expect("expected field must be present")
        .parse()
        .expect("identity field must be numeric")
}
