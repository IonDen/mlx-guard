#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_core::{
    Action, Event, LaunchErrorKind, LaunchOptions, OwnedProcess, PolicyConfig, PolicyMachine,
    RootOutcome, SignalNumber, SignalResult, StdioMode, validate_noninteractive_terminal,
};

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
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

fn signal_number(value: i32) -> SignalNumber {
    SignalNumber::new(u8::try_from(value).unwrap()).unwrap()
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
    let deadline = Instant::now() + Duration::from_secs(2);
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
fn launch_failures_and_interactive_terminal_are_rejected_before_work() {
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
    assert_eq!(
        validate_noninteractive_terminal(slave.is_terminal())
            .unwrap_err()
            .kind(),
        LaunchErrorKind::InteractiveTerminalUnsupported
    );
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
        .negotiate_checkpoint_endpoint(process.root_pid())
        .unwrap();
    assert!(
        process
            .negotiate_checkpoint_endpoint(std::process::id())
            .is_err()
    );
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
