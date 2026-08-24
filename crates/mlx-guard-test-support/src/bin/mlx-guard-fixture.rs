#![allow(unsafe_code)]

use std::env;
use std::fs::{File, OpenOptions};
use std::hint::black_box;
use std::io::{self, BufRead, Read, Write};
use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::process::{Command, Stdio};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use mlx_guard_core::{
    CHECKPOINT_FD_ENV, CheckpointAcknowledgement, CheckpointHello, CheckpointNonce,
    CheckpointRequest, CheckpointWorkerStatus, MAX_CHECKPOINT_FRAME_BYTES,
};
use mlx_guard_test_support::FixtureLimits;

const USAGE_EXIT: u8 = 64;
const PROTOCOL_EXIT: u8 = 65;
const FIXTURE_EXIT: u8 = 70;
const ARTIFACT_EXIT: u8 = 74;
const TIMEOUT_EXIT: i32 = 124;

static CHECKPOINT_REQUESTED: AtomicBool = AtomicBool::new(false);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.message);
            ExitCode::from(error.code)
        }
    }
}

fn run() -> Result<(), RunError> {
    let mut args = env::args().skip(1);
    let mode = args.next().ok_or_else(|| RunError::usage("missing mode"))?;
    let allocation_bytes = parse_u64(args.next(), "bytes")?;
    let wall_ms = parse_u64(args.next(), "wall milliseconds")?;
    if args.next().is_some() {
        return Err(RunError::usage("too many arguments"));
    }

    let limits = FixtureLimits::new(allocation_bytes, Duration::from_millis(wall_ms))
        .map_err(|error| RunError::usage(error.to_string()))?;
    start_watchdog(limits.wall_time());

    match mode.as_str() {
        "allocate" => run_allocate(limits),
        "shared" => run_shared(limits),
        "shared-live" => run_shared_live(limits),
        "ramp" => run_ramp(limits),
        "cpu-stall" => run_cpu_stall(limits),
        "fanout-stall" => run_fanout_stall(limits),
        "idle-stall" => run_idle_stall(),
        "spawn-churn" => run_spawn_churn(),
        "double-fork" => run_double_fork(limits),
        "double-fork-intermediate" => run_double_fork_intermediate(limits),
        "setsid-parent" => run_setsid_parent(limits),
        "setsid-stall" => run_setsid_stall(),
        "fast-root-exit" => run_fast_root_exit(limits),
        "term-then-exit" => run_term_then_exit(limits),
        "checkpoint-parent" => run_checkpoint_parent(limits),
        "checkpoint-success" => run_checkpoint_worker("success", limits),
        "checkpoint-blocked" => run_checkpoint_worker("blocked", limits),
        "checkpoint-exit" => run_checkpoint_worker("exit", limits),
        "checkpoint-cancel" => run_checkpoint_worker("cancel", limits),
        "checkpoint-allocate" => run_checkpoint_worker("allocate", limits),
        "checkpoint-spoof" => run_checkpoint_worker("spoof", limits),
        "checkpoint-exit-before-ready" => run_checkpoint_worker("exit-before-ready", limits),
        "checkpoint-hang-before-ready" => run_checkpoint_worker("hang-before-ready", limits),
        "short-exit" => Ok(()),
        "setsid" => run_setsid(),
        "ignore-term" => run_ignore_term(),
        "raw-fd" => run_raw_fd(),
        "artifact-failure" => run_artifact_failure(),
        _ => Err(RunError::usage(format!("unknown fixture mode {mode:?}"))),
    }
}

fn parse_u64(value: Option<String>, name: &str) -> Result<u64, RunError> {
    value
        .ok_or_else(|| RunError::usage(format!("missing {name}")))?
        .parse()
        .map_err(|_| RunError::usage(format!("invalid {name}")))
}

fn start_watchdog(wall_time: Duration) {
    thread::spawn(move || {
        thread::sleep(wall_time);
        eprintln!("fixture wall-time ceiling reached");
        std::process::exit(TIMEOUT_EXIT);
    });
}

fn run_allocate(limits: FixtureLimits) -> Result<(), RunError> {
    write_phase(&format!(
        "READY mode=allocate bytes={}",
        limits.allocation_bytes()
    ))?;
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    expect_command(&mut lines, "allocate")?;
    let length = usize::try_from(limits.allocation_bytes())
        .map_err(|_| RunError::fixture("allocation does not fit usize"))?;
    let allocation = AnonymousMapping::new(length)?;
    black_box(&allocation);
    write_phase("ALLOCATED")?;

    expect_command(&mut lines, "release")?;
    drop(allocation);
    write_phase("RELEASED")?;

    expect_command(&mut lines, "exit")?;
    write_phase("EXIT")
}

fn run_shared(limits: FixtureLimits) -> Result<(), RunError> {
    write_phase(&format!(
        "READY mode=shared bytes={}",
        limits.allocation_bytes()
    ))?;
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    expect_command(&mut lines, "allocate")?;
    let length = usize::try_from(limits.allocation_bytes())
        .map_err(|_| RunError::fixture("allocation does not fit usize"))?;
    let mapping = SharedMapping::new(length)?;
    mapping.verify_shared_after_fork()?;
    write_phase("ALLOCATED")?;

    expect_command(&mut lines, "release")?;
    drop(mapping);
    write_phase("RELEASED")?;

    expect_command(&mut lines, "exit")?;
    write_phase("EXIT")
}

fn run_shared_live(limits: FixtureLimits) -> Result<(), RunError> {
    let length = usize::try_from(limits.allocation_bytes())
        .map_err(|_| RunError::fixture("allocation does not fit usize"))?;
    let mapping = SharedMapping::new_uninitialized(length)?;
    let mut child = mapping.spawn_live_child()?;
    write_phase(&format!(
        "READY mode=shared-live bytes={}",
        limits.allocation_bytes()
    ))?;
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    expect_command(&mut lines, "allocate")?;
    mapping.fill(0xA5);
    write_phase("ALLOCATED")?;

    expect_command(&mut lines, "release")?;
    child.finish()?;
    drop(mapping);
    write_phase("RELEASED")?;

    expect_command(&mut lines, "exit")?;
    write_phase("EXIT")
}

fn run_ramp(limits: FixtureLimits) -> Result<(), RunError> {
    const RATE_BYTES_PER_SECOND: u64 = 128 * 1024 * 1024;
    const BASELINE_DELAY: Duration = Duration::from_millis(100);

    let length = usize::try_from(limits.allocation_bytes())
        .map_err(|_| RunError::fixture("allocation does not fit usize"))?;
    let allocation = AnonymousMapping::new_uninitialized(length)?;
    write_phase(&format!(
        "READY mode=ramp rate_bytes_per_second={RATE_BYTES_PER_SECOND} ceiling_bytes={length}"
    ))?;
    thread::sleep(BASELINE_DELAY);
    let started = Instant::now();
    let mut touched = 0_usize;
    while touched < length {
        let target = usize::try_from(
            (started.elapsed().as_nanos() * u128::from(RATE_BYTES_PER_SECOND) / 1_000_000_000)
                .min(length as u128),
        )
        .unwrap();
        if target > touched {
            allocation.fill_range(touched, target, 0xA5);
            touched = target;
            black_box(&allocation);
        }
        thread::sleep(Duration::from_millis(1));
    }
    loop {
        thread::park();
    }
}

fn run_cpu_stall(limits: FixtureLimits) -> Result<(), RunError> {
    write_phase(&format!(
        "READY mode=cpu-stall bytes={}",
        limits.allocation_bytes()
    ))?;
    let work_time = limits.wall_time().saturating_sub(Duration::from_millis(5));
    let deadline = Instant::now() + work_time;
    let mut iterations = 0_u64;
    while Instant::now() < deadline {
        iterations = black_box(iterations.wrapping_add(1));
    }
    write_phase(&format!("DONE iterations={iterations}"))
}

fn run_fanout_stall(limits: FixtureLimits) -> Result<(), RunError> {
    let member_count = usize::try_from(limits.allocation_bytes())
        .map_err(|_| RunError::fixture("member count does not fit usize"))?;
    if !(2..=16).contains(&member_count) {
        return Err(RunError::usage("fanout member count must be within 2..=16"));
    }
    let executable = env::current_exe()
        .map_err(|error| RunError::fixture(format!("current executable unavailable: {error}")))?;
    let wall_ms = limits.wall_time().as_millis().to_string();
    let mut children = Vec::with_capacity(member_count - 1);
    for _ in 1..member_count {
        let child = Command::new(&executable)
            .args(["idle-stall", "1", &wall_ms])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| RunError::fixture(format!("fanout child failed: {error}")))?;
        children.push(child);
    }
    black_box(&children);
    write_phase(&format!("READY mode=fanout-stall members={member_count}"))?;
    loop {
        thread::park();
    }
}

fn run_idle_stall() -> Result<(), RunError> {
    loop {
        thread::park();
    }
}

fn run_spawn_churn() -> Result<(), RunError> {
    write_phase("READY mode=spawn-churn")?;
    let executable = env::current_exe()
        .map_err(|error| RunError::fixture(format!("current executable unavailable: {error}")))?;
    for _ in 0..6 {
        let status = Command::new(&executable)
            .args(["cpu-stall", "1", "50"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| RunError::fixture(format!("churn child failed: {error}")))?;
        if !status.success() {
            return Err(RunError::fixture(format!(
                "churn child exited with {status}"
            )));
        }
    }
    write_phase("CHURNED children=6")
}

fn run_double_fork(limits: FixtureLimits) -> Result<(), RunError> {
    let executable = env::current_exe()
        .map_err(|error| RunError::fixture(format!("current executable unavailable: {error}")))?;
    let wall_ms = limits.wall_time().as_millis().to_string();
    let intermediate = Command::new(executable)
        .args(["double-fork-intermediate", "1", &wall_ms])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| RunError::fixture(format!("intermediate failed: {error}")))?;
    let intermediate_pid = intermediate.id();
    let output = intermediate
        .wait_with_output()
        .map_err(|error| RunError::fixture(format!("intermediate wait failed: {error}")))?;
    if !output.status.success() {
        return Err(RunError::fixture("intermediate did not exit normally"));
    }
    let output = String::from_utf8(output.stdout)
        .map_err(|_| RunError::fixture("intermediate output was not UTF-8"))?;
    let child_pid = output
        .trim()
        .strip_prefix("GRANDCHILD pid=")
        .ok_or_else(|| RunError::fixture("intermediate omitted grandchild PID"))?
        .parse::<u32>()
        .map_err(|_| RunError::fixture("grandchild PID was not numeric"))?;
    write_phase(&format!(
        "REPARENTED pid={child_pid} intermediate_pid={intermediate_pid}"
    ))?;
    loop {
        thread::park();
    }
}

fn run_double_fork_intermediate(limits: FixtureLimits) -> Result<(), RunError> {
    let executable = env::current_exe()
        .map_err(|error| RunError::fixture(format!("current executable unavailable: {error}")))?;
    let wall_ms = limits.wall_time().as_millis().to_string();
    let child = Command::new(executable)
        .args(["cpu-stall", "1", &wall_ms])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| RunError::fixture(format!("grandchild failed: {error}")))?;
    write_phase(&format!("GRANDCHILD pid={}", child.id()))
}

fn run_setsid_parent(limits: FixtureLimits) -> Result<(), RunError> {
    let executable = env::current_exe()
        .map_err(|error| RunError::fixture(format!("current executable unavailable: {error}")))?;
    let wall_ms = limits.wall_time().as_millis().to_string();
    let mut child = Command::new(executable)
        .args(["setsid-stall", "1", &wall_ms])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| RunError::fixture(format!("setsid child failed: {error}")))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| RunError::fixture("setsid stdout was not piped"))?;
    let ready = io::BufReader::new(output)
        .lines()
        .next()
        .ok_or_else(|| RunError::fixture("setsid child omitted identity"))?
        .map_err(|error| RunError::fixture(format!("setsid output failed: {error}")))?;
    write_phase(&ready)?;
    loop {
        thread::park();
    }
}

fn run_setsid_stall() -> Result<(), RunError> {
    // SAFETY: setsid has no pointer arguments; failure is checked before identity getters.
    if unsafe { libc::setsid() } == -1 {
        return Err(RunError::fixture(format!(
            "setsid failed: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: these identity getters have no preconditions.
    let (process_id, group_id, parent_id) =
        unsafe { (libc::getpid(), libc::getpgrp(), libc::getppid()) };
    write_phase(&format!(
        "ESCAPED pid={process_id} pgid={group_id} parent_pid={parent_id}"
    ))?;
    loop {
        thread::park();
    }
}

fn run_fast_root_exit(limits: FixtureLimits) -> Result<(), RunError> {
    let executable = env::current_exe()
        .map_err(|error| RunError::fixture(format!("current executable unavailable: {error}")))?;
    let child_wall_ms = limits.wall_time().as_millis().to_string();
    let child_mode = if limits.allocation_bytes() == 2 {
        "ignore-term"
    } else {
        "cpu-stall"
    };
    let mut command = Command::new(executable);
    command
        .args([child_mode, "1", &child_wall_ms])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if child_mode == "ignore-term" {
        // The child installs SIG_IGN after exec; the root must observe its readiness before
        // exiting so a SIGTERM sent immediately afterward cannot race the handler installation.
        command.stdout(Stdio::piped());
    } else {
        command.stdout(Stdio::null());
    }
    let mut child = command
        .spawn()
        .map_err(|error| RunError::fixture(format!("fast-exit child failed: {error}")))?;
    if child_mode == "ignore-term" {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RunError::fixture("ignore-term child stdout was not piped"))?;
        let ready = io::BufReader::new(stdout)
            .lines()
            .next()
            .ok_or_else(|| RunError::fixture("ignore-term child omitted readiness"))?
            .map_err(|error| {
                RunError::fixture(format!("ignore-term child output failed: {error}"))
            })?;
        if !ready.starts_with("READY ") {
            return Err(RunError::fixture(format!(
                "ignore-term child readiness was unexpected: {ready:?}"
            )));
        }
    }
    write_phase(&format!("CHILD pid={}", child.id()))?;
    if let Some(pid_file) = env::var_os("MLX_GUARD_FIXTURE_PID_FILE") {
        let mut file = File::create(&pid_file)
            .map_err(|error| RunError::fixture(format!("pid file create failed: {error}")))?;
        writeln!(file, "{}", child.id())
            .map_err(|error| RunError::fixture(format!("pid file write failed: {error}")))?;
    }
    std::process::exit(23);
}

static TERM_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn term_then_exit_signal_handler(_signal: libc::c_int) {
    TERM_RECEIVED.store(true, Ordering::SeqCst);
}

fn run_term_then_exit(limits: FixtureLimits) -> Result<(), RunError> {
    // SAFETY: the handler only stores to a process-local atomic flag and SIGTERM is a valid signal
    // number with a well-defined default disposition that this call replaces exactly once.
    if unsafe {
        libc::signal(
            libc::SIGTERM,
            term_then_exit_signal_handler as *const () as libc::sighandler_t,
        )
    } == libc::SIG_ERR
    {
        return Err(RunError::fixture(format!(
            "term-then-exit signal setup failed: {}",
            io::Error::last_os_error()
        )));
    }
    write_phase("READY mode=term-then-exit")?;
    while !TERM_RECEIVED.load(Ordering::SeqCst) {
        thread::park_timeout(Duration::from_millis(1));
    }
    thread::sleep(Duration::from_millis(limits.allocation_bytes()));
    std::process::exit(3);
}

fn run_checkpoint_parent(limits: FixtureLimits) -> Result<(), RunError> {
    let executable = env::current_exe()
        .map_err(|error| RunError::fixture(format!("current executable unavailable: {error}")))?;
    let child = Command::new(executable)
        .args([
            "cpu-stall",
            "1",
            &limits.wall_time().as_millis().to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| RunError::fixture(format!("checkpoint child failed: {error}")))?;
    write_phase(&format!("CHILD pid={}", child.id()))?;
    loop {
        thread::park();
    }
}

extern "C" fn checkpoint_signal_handler(_signal: libc::c_int) {
    CHECKPOINT_REQUESTED.store(true, Ordering::SeqCst);
}

fn run_checkpoint_worker(mode: &str, limits: FixtureLimits) -> Result<(), RunError> {
    let fd: i32 = env::var(CHECKPOINT_FD_ENV)
        .map_err(|_| RunError::usage("checkpoint descriptor is required"))?
        .parse()
        .map_err(|_| RunError::usage("checkpoint descriptor must be numeric"))?;
    // SAFETY: the supervisor passes this process ownership of one inherited socket descriptor.
    let mut channel = unsafe { File::from_raw_fd(fd) };
    let hello_frame = read_checkpoint_frame(&mut channel)?
        .ok_or_else(|| RunError::protocol("checkpoint hello ended before a frame"))?;
    let hello = CheckpointHello::decode(&hello_frame)
        .map_err(|error| RunError::protocol(error.to_string()))?;
    if mode == "exit-before-ready" {
        return Ok(());
    }
    if mode == "hang-before-ready" {
        loop {
            thread::park();
        }
    }
    CHECKPOINT_REQUESTED.store(false, Ordering::SeqCst);
    // SAFETY: the handler only stores to a process-local atomic flag and SIGUSR1 is validated by the
    // checkpoint protocol configuration.
    if unsafe {
        libc::signal(
            libc::SIGUSR1,
            checkpoint_signal_handler as *const () as libc::sighandler_t,
        )
    } == libc::SIG_ERR
    {
        return Err(RunError::fixture(format!(
            "checkpoint signal setup failed: {}",
            io::Error::last_os_error()
        )));
    }
    channel
        .write_all(&hello.ready_frame())
        .and_then(|()| channel.flush())
        .map_err(|error| RunError::protocol(format!("checkpoint ready failed: {error}")))?;
    write_phase(&format!("READY mode=checkpoint-{mode}"))?;
    while !CHECKPOINT_REQUESTED.load(Ordering::SeqCst) {
        thread::park_timeout(Duration::from_millis(1));
    }
    if mode == "exit" {
        return Ok(());
    }
    let Some(request) = read_checkpoint_request(&mut channel)? else {
        if mode == "cancel" {
            return write_phase("CANCELLED");
        }
        return Err(RunError::protocol(
            "checkpoint request ended before a frame",
        ));
    };
    if mode == "blocked" {
        thread::sleep(Duration::from_millis(limits.allocation_bytes()));
    }
    let allocation = if mode == "allocate" {
        let length = usize::try_from(limits.allocation_bytes())
            .map_err(|_| RunError::fixture("checkpoint allocation does not fit usize"))?;
        Some(AnonymousMapping::new(length)?)
    } else {
        None
    };
    if mode == "spoof" {
        let spoof = CheckpointAcknowledgement::new(
            CheckpointNonce::from_bytes([8; 32]),
            request.request_id(),
            CheckpointWorkerStatus::Completed,
            None,
        )
        .encode();
        channel
            .write_all(&spoof)
            .and_then(|()| channel.flush())
            .map_err(|error| RunError::protocol(format!("checkpoint spoof failed: {error}")))?;
        thread::sleep(Duration::from_millis(20));
    }
    let acknowledgement =
        CheckpointAcknowledgement::for_request(&request, CheckpointWorkerStatus::Completed, None)
            .encode();
    channel
        .write_all(&acknowledgement)
        .and_then(|()| channel.flush())
        .map_err(|error| RunError::protocol(format!("checkpoint ack failed: {error}")))?;
    black_box(&allocation);
    loop {
        thread::park();
    }
}

fn read_checkpoint_request(channel: &mut File) -> Result<Option<CheckpointRequest>, RunError> {
    let Some(frame) = read_checkpoint_frame(channel)? else {
        return Ok(None);
    };
    CheckpointRequest::decode(&frame)
        .map(Some)
        .map_err(|error| RunError::protocol(error.to_string()))
}

fn read_checkpoint_frame(channel: &mut File) -> Result<Option<Vec<u8>>, RunError> {
    let mut header = [0_u8; 4];
    match channel.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => {
            return Err(RunError::protocol(format!(
                "checkpoint request header failed: {error}"
            )));
        }
    }
    let body_len = u32::from_be_bytes(header) as usize;
    if body_len > MAX_CHECKPOINT_FRAME_BYTES {
        return Err(RunError::protocol("checkpoint request was oversized"));
    }
    let mut frame = Vec::with_capacity(4 + body_len);
    frame.extend_from_slice(&header);
    frame.resize(4 + body_len, 0);
    channel
        .read_exact(&mut frame[4..])
        .map_err(|error| RunError::protocol(format!("checkpoint request body failed: {error}")))?;
    Ok(Some(frame))
}

fn run_setsid() -> Result<(), RunError> {
    // SAFETY: setsid has no pointer arguments; failure is checked before reading identity values.
    if unsafe { libc::setsid() } == -1 {
        return Err(RunError::fixture(format!(
            "setsid failed: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: these identity getters have no preconditions and cannot invalidate memory.
    let (process_id, group_id, session_id) =
        unsafe { (libc::getpid(), libc::getpgrp(), libc::getsid(0)) };
    write_phase(&format!(
        "ESCAPED pid={process_id} pgid={group_id} sid={session_id}"
    ))
}

fn run_ignore_term() -> Result<(), RunError> {
    // SAFETY: SIG_IGN is a valid signal disposition and SIGTERM is a valid signal number.
    if unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) } == libc::SIG_ERR {
        return Err(RunError::fixture(format!(
            "signal failed: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: getpid has no preconditions.
    write_phase(&format!("READY mode=ignore-term pid={}", unsafe {
        libc::getpid()
    }))?;
    loop {
        thread::park();
    }
}

fn run_raw_fd() -> Result<(), RunError> {
    let fd: i32 = env::var("MLX_GUARD_FIXTURE_FD")
        .map_err(|_| RunError::usage("MLX_GUARD_FIXTURE_FD is required"))?
        .parse()
        .map_err(|_| RunError::usage("MLX_GUARD_FIXTURE_FD must be numeric"))?;
    // SAFETY: the harness passes an owned inherited descriptor and does not use it in the child.
    let mut output = unsafe { File::from_raw_fd(fd) };
    output
        .write_all(b"\0\0\0\x0ffixture-frame\0\xff")
        .and_then(|()| output.flush())
        .map_err(|error| RunError::fixture(format!("raw frame failed: {error}")))?;
    write_phase("DONE mode=raw-fd")
}

fn run_artifact_failure() -> Result<(), RunError> {
    let path = PathBuf::from(
        env::var_os("MLX_GUARD_FIXTURE_PATH")
            .ok_or_else(|| RunError::usage("MLX_GUARD_FIXTURE_PATH is required"))?,
    );
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => {
            drop(file);
            std::fs::remove_file(&path).map_err(|error| {
                RunError::artifact(format!(
                    "artifact cleanup failed for {}: {error}",
                    path.display()
                ))
            })?;
            Err(RunError::artifact(format!(
                "artifact create unexpectedly succeeded: {}",
                path.display()
            )))
        }
        Err(error) => Err(RunError::artifact(format!(
            "artifact create failed for {}: {error}",
            path.display()
        ))),
    }
}

fn expect_command<I>(lines: &mut I, expected: &str) -> Result<(), RunError>
where
    I: Iterator<Item = io::Result<String>>,
{
    match lines.next() {
        Some(Ok(command)) if command == expected => Ok(()),
        Some(Ok(command)) => Err(RunError::protocol(format!(
            "expected {expected:?}, got {command:?}"
        ))),
        Some(Err(error)) => Err(RunError::protocol(format!("stdin failed: {error}"))),
        None => Err(RunError::protocol(format!(
            "expected {expected:?}, got EOF"
        ))),
    }
}

fn write_phase(value: &str) -> Result<(), RunError> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    writeln!(lock, "{value}").map_err(|error| RunError::fixture(error.to_string()))?;
    lock.flush()
        .map_err(|error| RunError::fixture(error.to_string()))
}

struct RunError {
    code: u8,
    message: String,
}

impl RunError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: USAGE_EXIT,
            message: message.into(),
        }
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self {
            code: PROTOCOL_EXIT,
            message: message.into(),
        }
    }

    fn fixture(message: impl Into<String>) -> Self {
        Self {
            code: FIXTURE_EXIT,
            message: message.into(),
        }
    }

    fn artifact(message: impl Into<String>) -> Self {
        Self {
            code: ARTIFACT_EXIT,
            message: message.into(),
        }
    }
}

struct SharedMapping {
    address: *mut libc::c_void,
    length: usize,
}

struct LiveSharedChild {
    pid: libc::pid_t,
    release_fd: libc::c_int,
    finished: bool,
}

struct AnonymousMapping {
    address: *mut libc::c_void,
    length: usize,
}

impl AnonymousMapping {
    fn new(length: usize) -> Result<Self, RunError> {
        let mapping = Self::new_uninitialized(length)?;
        mapping.fill_range(0, length, 0xA5);
        Ok(mapping)
    }

    fn new_uninitialized(length: usize) -> Result<Self, RunError> {
        // SAFETY: arguments describe a new private anonymous mapping; failure is checked.
        let address = unsafe {
            libc::mmap(
                ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(RunError::fixture(format!(
                "mmap failed: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(Self { address, length })
    }

    fn fill_range(&self, start: usize, end: usize, value: u8) {
        assert!(start <= end && end <= self.length);
        // SAFETY: the asserted range lies within the writable mapping.
        unsafe {
            ptr::write_bytes(
                self.address.cast::<u8>().add(start),
                value,
                end.saturating_sub(start),
            );
        }
    }
}

impl Drop for AnonymousMapping {
    fn drop(&mut self) {
        // SAFETY: the address and length come from the successful mmap and are unmapped once here.
        let result = unsafe { libc::munmap(self.address, self.length) };
        if result != 0 {
            eprintln!("munmap failed: {}", io::Error::last_os_error());
        }
    }
}

impl SharedMapping {
    fn new(length: usize) -> Result<Self, RunError> {
        let mapping = Self::new_uninitialized(length)?;
        mapping.fill(0xA5);
        Ok(mapping)
    }

    fn new_uninitialized(length: usize) -> Result<Self, RunError> {
        // SAFETY: arguments describe a new anonymous mapping; MAP_FAILED is checked before writes.
        let address = unsafe {
            libc::mmap(
                ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(RunError::fixture(format!(
                "mmap failed: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(Self { address, length })
    }

    fn fill(&self, value: u8) {
        // SAFETY: mmap returned a writable region of exactly `length` bytes.
        unsafe { ptr::write_bytes(self.address.cast::<u8>(), value, self.length) };
    }

    fn verify_shared_after_fork(&self) -> Result<(), RunError> {
        // SAFETY: after fork the child performs only one mapped-byte write and `_exit`, both valid
        // in the post-fork child of this multithreaded fixture. The parent checks waitpid.
        let child = unsafe { libc::fork() };
        if child == -1 {
            return Err(RunError::fixture(format!(
                "fork failed: {}",
                io::Error::last_os_error()
            )));
        }
        if child == 0 {
            // SAFETY: mmap returned at least one writable byte and the child exits immediately.
            unsafe {
                self.address.cast::<u8>().write(0x5A);
                libc::_exit(0);
            }
        }
        let mut status = 0;
        // SAFETY: `child` is the live PID returned by fork and `status` is writable.
        if unsafe { libc::waitpid(child, &raw mut status, 0) } != child || !libc::WIFEXITED(status)
        {
            return Err(RunError::fixture("shared-mapping child failed"));
        }
        // SAFETY: the mapping remains live and writable for its stored length.
        if unsafe { self.address.cast::<u8>().read() } != 0x5A {
            return Err(RunError::fixture(
                "mapping mutation was not shared across fork",
            ));
        }
        Ok(())
    }

    fn spawn_live_child(&self) -> Result<LiveSharedChild, RunError> {
        let mut ready_pipe = [-1; 2];
        let mut release_pipe = [-1; 2];
        // SAFETY: both arrays provide storage for the two descriptors written by pipe.
        if unsafe { libc::pipe(ready_pipe.as_mut_ptr()) } != 0 {
            return Err(RunError::fixture(format!(
                "ready pipe failed: {}",
                io::Error::last_os_error()
            )));
        }
        // SAFETY: both arrays provide storage for the two descriptors written by pipe.
        if unsafe { libc::pipe(release_pipe.as_mut_ptr()) } != 0 {
            close_fd(ready_pipe[0]);
            close_fd(ready_pipe[1]);
            return Err(RunError::fixture(format!(
                "release pipe failed: {}",
                io::Error::last_os_error()
            )));
        }

        // SAFETY: the child uses only async-signal-safe libc calls before `_exit`.
        let child = unsafe { libc::fork() };
        if child == -1 {
            for fd in ready_pipe.into_iter().chain(release_pipe) {
                close_fd(fd);
            }
            return Err(RunError::fixture(format!(
                "fork failed: {}",
                io::Error::last_os_error()
            )));
        }
        if child == 0 {
            // SAFETY: these descriptors belong to this process, the mapping has at least one byte,
            // and all operations below are async-signal-safe after fork.
            unsafe {
                libc::close(ready_pipe[0]);
                libc::close(release_pipe[1]);
                self.address.cast::<u8>().write(0x5A);
                let ready = [1_u8];
                if libc::write(ready_pipe[1], ready.as_ptr().cast(), 1) != 1 {
                    libc::_exit(70);
                }
                libc::close(ready_pipe[1]);
                let mut release = [0_u8];
                let read = libc::read(release_pipe[0], release.as_mut_ptr().cast(), 1);
                libc::close(release_pipe[0]);
                libc::_exit(if read >= 0 { 0 } else { 70 });
            }
        }

        close_fd(ready_pipe[1]);
        close_fd(release_pipe[0]);
        let mut ready = [0_u8];
        // SAFETY: ready_pipe[0] is an open read descriptor and `ready` is writable.
        let ready_bytes = unsafe { libc::read(ready_pipe[0], ready.as_mut_ptr().cast(), 1) };
        close_fd(ready_pipe[0]);
        // SAFETY: the mapping remains live and writable for its stored length.
        let shared_byte = unsafe { self.address.cast::<u8>().read() };
        if ready_bytes != 1 || ready[0] != 1 || shared_byte != 0x5A {
            close_fd(release_pipe[1]);
            wait_for_child(child);
            return Err(RunError::fixture("live shared-mapping child failed"));
        }
        Ok(LiveSharedChild {
            pid: child,
            release_fd: release_pipe[1],
            finished: false,
        })
    }
}

impl LiveSharedChild {
    fn finish(&mut self) -> Result<(), RunError> {
        if self.finished {
            return Ok(());
        }
        let release = [1_u8];
        // SAFETY: release_fd is the open write side retained by the parent.
        let written = unsafe { libc::write(self.release_fd, release.as_ptr().cast(), 1) };
        close_fd(self.release_fd);
        self.release_fd = -1;
        let exited_cleanly = wait_for_child(self.pid);
        self.finished = true;
        if written != 1 || !exited_cleanly {
            return Err(RunError::fixture("live shared-mapping child failed"));
        }
        Ok(())
    }
}

impl Drop for LiveSharedChild {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        close_fd(self.release_fd);
        self.release_fd = -1;
        wait_for_child(self.pid);
        self.finished = true;
    }
}

fn close_fd(fd: libc::c_int) {
    if fd >= 0 {
        // SAFETY: callers pass a descriptor they own and close at most once.
        unsafe {
            libc::close(fd);
        }
    }
}

fn wait_for_child(pid: libc::pid_t) -> bool {
    let mut status = 0;
    // SAFETY: pid came from a successful fork and status is writable.
    unsafe { libc::waitpid(pid, &raw mut status, 0) == pid && libc::WIFEXITED(status) }
}

impl Drop for SharedMapping {
    fn drop(&mut self) {
        // SAFETY: the address and length come from the successful mmap and are unmapped once here.
        let result = unsafe { libc::munmap(self.address, self.length) };
        if result != 0 {
            eprintln!("munmap failed: {}", io::Error::last_os_error());
        }
    }
}
