#![allow(unsafe_code)]

use std::env;
use std::fs::{File, OpenOptions};
use std::hint::black_box;
use std::io::{self, BufRead, Write};
use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::process::{Command, Stdio};
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use mlx_guard_test_support::FixtureLimits;

const USAGE_EXIT: u8 = 64;
const PROTOCOL_EXIT: u8 = 65;
const FIXTURE_EXIT: u8 = 70;
const ARTIFACT_EXIT: u8 = 74;
const TIMEOUT_EXIT: i32 = 124;

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
        "cpu-stall" => run_cpu_stall(limits),
        "spawn-churn" => run_spawn_churn(),
        "fast-root-exit" => run_fast_root_exit(limits),
        "checkpoint-parent" => run_checkpoint_parent(limits),
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
    let mut allocation = Vec::new();
    allocation
        .try_reserve_exact(length)
        .map_err(|error| RunError::fixture(format!("allocation failed: {error}")))?;
    allocation.resize(length, 0);
    for index in (0..length).step_by(4096) {
        allocation[index] = 0xA5;
    }
    allocation[length - 1] = 0xA5;
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

fn run_spawn_churn() -> Result<(), RunError> {
    write_phase("READY mode=spawn-churn")?;
    let executable = env::current_exe()
        .map_err(|error| RunError::fixture(format!("current executable unavailable: {error}")))?;
    for _ in 0..6 {
        let status = Command::new(&executable)
            .args(["short-exit", "1", "50"])
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

fn run_fast_root_exit(limits: FixtureLimits) -> Result<(), RunError> {
    let executable = env::current_exe()
        .map_err(|error| RunError::fixture(format!("current executable unavailable: {error}")))?;
    let child_wall_ms = limits.wall_time().as_millis().to_string();
    let child = Command::new(executable)
        .args(["cpu-stall", "1", &child_wall_ms])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| RunError::fixture(format!("fast-exit child failed: {error}")))?;
    write_phase(&format!("CHILD pid={}", child.id()))?;
    std::process::exit(23);
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

impl SharedMapping {
    fn new(length: usize) -> Result<Self, RunError> {
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
        // SAFETY: mmap returned a writable region of exactly `length` bytes.
        unsafe { ptr::write_bytes(address.cast::<u8>(), 0xA5, length) };
        Ok(Self { address, length })
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
