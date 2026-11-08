#![cfg(unix)]
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_core::{
    Action, Event, LaunchErrorKind, LaunchOptions, NativeProcessInventory, OwnedProcess,
    PolicyConfig, PolicyMachine, RootOutcome, SignalNumber, SignalResult, StdinDisposition,
    StdioMode, resolve_stdin,
};
use mlx_guard_test_support::root_identity;

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");

fn fixture(mode: &str, wall_ms: u64) -> LaunchOptions {
    LaunchOptions {
        command: vec![
            OsString::from(FIXTURE),
            OsString::from(mode),
            OsString::from("1"),
            OsString::from(wall_ms.to_string()),
        ],
        cwd: None,
        clear_env: false,
        env: BTreeMap::new(),
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    }
}

fn read_all(mut file: impl Read) -> String {
    let mut value = String::new();
    file.read_to_string(&mut value).unwrap();
    value
}

fn process_exists(pid: i32) -> bool {
    // SAFETY: signal zero performs an identity/existence check and `pid` came from a live fixture.
    let exists = unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    };
    exists && !process_is_zombie(pid)
}

#[cfg(target_os = "linux")]
fn process_is_zombie(pid: i32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, fields)| fields.starts_with("Z "))
        })
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
const fn process_is_zombie(_pid: i32) -> bool {
    false
}

fn signal_number(value: i32) -> SignalNumber {
    SignalNumber::new(u8::try_from(value).unwrap()).unwrap()
}

/// Whether SIGINT is at `SIG_DFL` in this process, read without changing it.
fn sigint_is_at_default() -> bool {
    // SAFETY: `sigaction` is plain old data, so the all-zero value is a valid one.
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null new action only queries the disposition, and `current` is a valid, writable
    // `sigaction` for the call to fill in.
    let queried = unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), &raw mut current) };
    assert_eq!(queried, 0, "sigaction(SIGINT) query failed");
    current.sa_sigaction == libc::SIG_DFL
}

fn enforcement_policy() -> PolicyMachine {
    PolicyMachine::enforce(PolicyConfig {
        limit_bytes: 100,
        warning_bytes: 90,
        recovery_bytes: 80,
        emergency_bytes: 150,
        required_breach_samples: 2,
        max_missing_samples: 3,
        max_sample_age: Duration::from_millis(100),
        max_sample_window: Duration::from_millis(10),
        checkpoint_timeout: None,
        term_grace: Duration::from_millis(100),
        wall_time: None,
    })
    .unwrap()
}

fn wait_until_gone(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!process_exists(pid), "process {pid} survived group cleanup");
}

fn assert_stays_alive(pid: i32) {
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        assert!(
            process_exists(pid),
            "process {pid} received an endpoint-only signal"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn launch_is_direct_and_applies_cwd_environment_and_redirects() {
    // Catches shell execution, ignored cwd/env controls, or accidentally inherited environment.
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let cwd = std::env::temp_dir().join(format!("mlx-guard-cwd-{}-{unique}", std::process::id()));
    fs::create_dir(&cwd).unwrap();
    let canonical_cwd = fs::canonicalize(&cwd).unwrap();

    let mut env = BTreeMap::new();
    env.insert(
        "MLX_GUARD_TEST_VALUE".to_owned(),
        "literal;$(touch should-not-exist)".to_owned(),
    );
    let options = LaunchOptions {
        command: vec![OsString::from("/usr/bin/env")],
        cwd: Some(cwd.clone()),
        clear_env: true,
        env,
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    };
    let mut process = OwnedProcess::launch(&options).unwrap();
    let stdout = read_all(process.take_stdout().unwrap());
    assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(0));
    assert_eq!(
        stdout,
        "MLX_GUARD_TEST_VALUE=literal;$(touch should-not-exist)\n"
    );

    let options = LaunchOptions {
        command: vec![
            OsString::from("/bin/echo"),
            OsString::from("literal;$(touch should-not-exist)"),
        ],
        cwd: Some(cwd.clone()),
        clear_env: false,
        env: BTreeMap::new(),
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    };
    let mut process = OwnedProcess::launch(&options).unwrap();
    let stdout = read_all(process.take_stdout().unwrap());
    assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(0));
    assert_eq!(stdout, "literal;$(touch should-not-exist)\n");
    assert!(!cwd.join("should-not-exist").exists());

    let options = LaunchOptions {
        command: vec![OsString::from("/bin/pwd")],
        cwd: Some(cwd.clone()),
        clear_env: false,
        env: BTreeMap::new(),
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    };
    let mut process = OwnedProcess::launch(&options).unwrap();
    let stdout = read_all(process.take_stdout().unwrap());
    assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(0));
    assert_eq!(stdout.trim_end(), canonical_cwd.to_str().unwrap());

    fs::remove_dir(cwd).unwrap();
}

#[test]
fn piped_stdio_round_trips_without_becoming_control_data() {
    // Catches stdio inheritance hardcoding or parsing child output as supervisor control frames.
    let mut options = fixture("allocate", 1_000);
    options.stdin = StdioMode::Piped;
    let mut process = OwnedProcess::launch(&options).unwrap();
    let mut input = process.take_stdin().unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "READY mode=allocate bytes=1\n");
    input.write_all(b"allocate\nrelease\nexit\n").unwrap();
    input.flush().unwrap();
    let remainder = read_all(output);
    assert_eq!(remainder, "ALLOCATED\nRELEASED\nEXIT\n");
    assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(0));
}

#[test]
fn launch_failures_are_rejected_before_work_and_a_terminal_becomes_dev_null() {
    // Catches collapsing not-found/not-executable/cwd/TTY failures into an ambiguous spawn error.
    let mut missing = fixture("short-exit", 100);
    missing.command[0] = OsString::from("/definitely/missing/mlx-guard-command");
    assert_eq!(
        OwnedProcess::launch(&missing).unwrap_err().kind(),
        LaunchErrorKind::NotFound
    );

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("mlx-guard-nonexec-{}-{unique}", std::process::id()));
    fs::write(&path, b"not executable").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut nonexec = fixture("short-exit", 100);
    nonexec.command[0] = path.clone().into_os_string();
    assert_eq!(
        OwnedProcess::launch(&nonexec).unwrap_err().kind(),
        LaunchErrorKind::NotExecutable
    );
    fs::remove_file(path).unwrap();

    let mut invalid_cwd = fixture("short-exit", 100);
    invalid_cwd.cwd = Some("/definitely/missing/mlx-guard-cwd".into());
    assert_eq!(
        OwnedProcess::launch(&invalid_cwd).unwrap_err().kind(),
        LaunchErrorKind::InvalidWorkingDirectory
    );

    let mut descriptors = [0; 2];
    // SAFETY: openpty initializes both descriptors; no terminal name or attributes are requested.
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
    let (_master, slave) = unsafe {
        (
            File::from_raw_fd(descriptors[0]),
            File::from_raw_fd(descriptors[1]),
        )
    };
    assert!(slave.is_terminal());
    // A terminal is not a launch failure any more: an inherited terminal becomes /dev/null, and
    // the caller is told so. Red if the substitution is dropped (Inherit comes back), if it is
    // applied to a non-terminal stdin, or if it touches an explicitly piped or null stdin.
    assert_eq!(
        resolve_stdin(StdioMode::Inherit, slave.is_terminal()),
        (StdioMode::Null, StdinDisposition::TerminalReplacedWithNull)
    );
    assert_eq!(
        resolve_stdin(StdioMode::Inherit, false),
        (StdioMode::Inherit, StdinDisposition::AsRequested)
    );
    for requested in [StdioMode::Piped, StdioMode::Null] {
        assert_eq!(
            resolve_stdin(requested, true),
            (requested, StdinDisposition::AsRequested)
        );
    }
}

#[test]
fn root_status_is_preserved_while_drop_cleans_a_remaining_group() {
    // Catches losing exit 23, waiting for descendants as the root, or abandoning the owned child.
    let mut process = OwnedProcess::launch(&fixture("fast-root-exit", 5_000)).unwrap();
    assert_eq!(process.process_group_id(), process.root_pid().cast_signed());
    let mut output = BufReader::new(process.take_stdout().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let child_pid: i32 = line
        .trim()
        .strip_prefix("CHILD pid=")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(23));
    assert!(process_exists(child_pid));
    drop(process);
    wait_until_gone(child_pid);
}

/// Kills and reaps, by pid, a root whose `OwnedProcess` is gone, so a test leaves no stray even
/// when it fails. It signals only while the pid is still an unreaped child of this process, so the
/// number cannot have been reused.
struct RootReaper(i32);

impl Drop for RootReaper {
    fn drop(&mut self) {
        let mut status = 0;
        // SAFETY: waitpid and kill take a pid and a status pointer to a live local. The kill is
        // sent only while waitpid reports the pid as this process's own unreaped child.
        unsafe {
            if libc::waitpid(self.0, &raw mut status, libc::WNOHANG) == 0 {
                libc::kill(self.0, libc::SIGKILL);
                libc::waitpid(self.0, &raw mut status, 0);
            }
        }
    }
}

#[test]
fn dropping_an_owned_process_does_not_wait_for_a_root_that_left_its_group() {
    // Catches `child.wait()` with no bound in `Drop`: the group kill misses a root that moved to
    // another group, so an unbounded wait lasts until the root's own 6 s watchdog ends it.
    let mut process = OwnedProcess::launch(&fixture("leave-group", 6_000)).unwrap();
    let root = process.root_pid().cast_signed();
    let reaper = RootReaper(root);
    let mut stderr = process.take_stderr().unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert!(
        line.starts_with("LEFT-GROUP pgid="),
        "fixture did not leave its group: {line:?} {:?}",
        read_all(&mut stderr)
    );
    let group: i32 = line
        .trim()
        .strip_prefix("LEFT-GROUP pgid=")
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(group, root, "the root is still the leader of its own group");

    let started = Instant::now();
    drop(process);
    let elapsed = started.elapsed();
    let alive = process_exists(root);
    assert!(
        elapsed < Duration::from_secs(4),
        "drop waited {elapsed:?} for a root out of the group kill's reach (alive after: {alive})"
    );
    assert!(
        alive,
        "the root was gone after the drop, so the drop outlasted it"
    );
    drop(reaper);
    drop(stderr);
}

#[test]
fn relinquishing_ownership_does_not_signal_the_process_group() {
    let process = OwnedProcess::launch(&fixture("cpu-stall", 2_000)).unwrap();
    let control = process.control_handle();

    process.relinquish();

    assert!(control.owned_group_exists().unwrap());
    assert_eq!(control.kill_group().unwrap(), SignalResult::Delivered);
}

#[test]
fn repeated_terminal_signal_routes_through_policy_to_group_kill() {
    // Catches signalling PID zero, signalling only the root, or ignoring repeated interruption.
    let mut process = OwnedProcess::launch(&fixture("ignore-term", 2_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert!(line.starts_with("READY mode=ignore-term pid="));
    let term = signal_number(libc::SIGTERM);
    let mut policy = enforcement_policy();
    assert_eq!(
        policy.apply(Event::ExternalSignal {
            at: Duration::from_millis(1),
            signal: term,
        }),
        [Action::ForwardSignal(term)]
    );
    assert_eq!(
        process.forward_terminal_signal(term).unwrap(),
        SignalResult::Delivered
    );
    thread::sleep(Duration::from_millis(20));
    assert_eq!(process.try_wait_root().unwrap(), None);
    assert_eq!(
        policy.apply(Event::ExternalSignal {
            at: Duration::from_millis(2),
            signal: term,
        }),
        [Action::SendKill]
    );
    assert_eq!(process.kill_group().unwrap(), SignalResult::Delivered);
    assert_eq!(
        process.wait_root().unwrap(),
        RootOutcome::Signaled(signal_number(libc::SIGKILL))
    );
}

#[test]
fn external_terminal_signal_is_forwarded_but_stop_is_not_supported() {
    // Catches rewriting the first signal to TERM or accidentally enabling unsupported job control.
    // The fixture inherits this process's SIGINT disposition across `exec`, and a shell running
    // the suite as a background job sets it to ignored, so the forwarded SIGINT would do nothing.
    // Bug the precondition catches: that case failing as an unexplained `Exited(0)`.
    assert!(
        sigint_is_at_default(),
        "SIGINT is not at its default disposition in the test process (a non-interactive shell \
         ignores it for a background job), so the fixture would ignore the forwarded SIGINT; run \
         the suite in the foreground"
    );
    let mut process = OwnedProcess::launch(&fixture("cpu-stall", 2_000)).unwrap();
    let interrupt = signal_number(libc::SIGINT);
    assert_eq!(
        process.forward_terminal_signal(interrupt).unwrap(),
        SignalResult::Delivered
    );
    assert_eq!(
        process.wait_root().unwrap(),
        RootOutcome::Signaled(interrupt)
    );

    let process = OwnedProcess::launch(&fixture("cpu-stall", 100)).unwrap();
    let stop = signal_number(libc::SIGTSTP);
    assert!(process.forward_terminal_signal(stop).is_err());
}

#[test]
fn checkpoint_signal_reaches_only_the_negotiated_endpoint() {
    // Catches implementing checkpoint delivery as a process-group broadcast.
    let mut process = OwnedProcess::launch(&fixture("checkpoint-parent", 2_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let child_pid: i32 = line
        .trim()
        .strip_prefix("CHILD pid=")
        .unwrap()
        .parse()
        .unwrap();
    let endpoint = process
        .negotiate_checkpoint_endpoint(root_identity(&process))
        .unwrap();
    // The test process is live and its identity is correctly inspected, so only its process group
    // differs from the owned one: negotiation still refuses it, proving group membership is checked
    // when the endpoint is negotiated and not left to delivery-time identity revalidation.
    let outsider = NativeProcessInventory::new()
        .inspect(std::process::id().cast_signed())
        .unwrap()
        .identity;
    assert!(process.negotiate_checkpoint_endpoint(outsider).is_err());
    let request = signal_number(libc::SIGUSR1);
    assert_eq!(
        process.signal_checkpoint(&endpoint, request).unwrap(),
        SignalResult::Delivered
    );
    assert_eq!(process.wait_root().unwrap(), RootOutcome::Signaled(request));
    assert_stays_alive(child_pid);
    drop(process);
    wait_until_gone(child_pid);
}

/// Wait at most 5 s for a fixture with a 300 ms wall limit; kill it and fail if it outlives that.
fn exit_within_ceiling(mut child: Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    panic!("fixture outlived its 300 ms wall-time ceiling by more than 4.7 s");
}

fn idle_fixture(stderr: Stdio) -> Child {
    Command::new(FIXTURE)
        .args(["idle-stall", "1", "300"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .unwrap()
}

#[test]
fn fixture_watchdog_ends_the_fixture_when_its_stderr_is_closed() {
    // Catches a watchdog whose notice panics on a closed stderr: the panic ends only the watchdog
    // thread, and the parked fixture then runs past its wall-time ceiling.
    let mut child = idle_fixture(Stdio::piped());
    drop(child.stderr.take());
    assert_eq!(exit_within_ceiling(child).code(), Some(124));
}

#[test]
fn fixture_watchdog_ends_the_fixture_when_its_stderr_is_full() {
    // Catches a watchdog whose notice blocks on a full stderr pipe before it can exit.
    let (reader, mut writer) = std::io::pipe().unwrap();
    let child_stderr = writer.try_clone().unwrap();
    // Fills the pipe and then blocks, so the fixture's write blocks too. Closing the reader below
    // ends this write with an error.
    let filler = thread::spawn(move || writer.write_all(&vec![0_u8; 1 << 20]).is_err());
    let child = idle_fixture(Stdio::from(child_stderr));
    let status = exit_within_ceiling(child);
    drop(reader);
    assert!(
        filler.join().unwrap(),
        "the filler wrote 1 MiB into an unread pipe"
    );
    assert_eq!(status.code(), Some(124));
}
