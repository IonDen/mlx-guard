//! Whatever happens to whoever reads stderr, supervision goes on: a full unread pipe must not stop
//! a footprint intervention, and a reader that has gone away must not change the exit code or lose
//! the report. Real processes and real pipes throughout, because a blocked `write(2)` and an `EPIPE`
//! are what is under test.

#![cfg(unix)]
#![allow(unsafe_code)]

use std::io::{self, PipeWriter};
use std::process::{Command, Stdio};

const GUARD: &str = env!("CARGO_BIN_EXE_mlx-guard");

/// A pipe whose reader closed before anyone could write to it, so every write gets `EPIPE`.
struct DeadPipe(PipeWriter);

impl DeadPipe {
    fn new() -> Self {
        let (reader, writer) = io::pipe().unwrap();
        drop(reader);
        Self(writer)
    }

    fn stdio(self) -> Stdio {
        Stdio::from(self.0)
    }
}

#[test]
fn a_dead_stderr_reader_keeps_exit_64_on_a_parse_error() {
    // Bug: the parse-error path writes with a panicking `eprint!`, so a broken pipe turns 64 into 101.
    let status = Command::new(GUARD)
        .args(["run", "--max-footprint=bogus", "--", "/usr/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(DeadPipe::new().stdio())
        .status()
        .expect("the supervisor must start");

    assert_eq!(status.code(), Some(64));
}

#[test]
fn a_dead_stdout_reader_keeps_exit_0_on_help() {
    // Bug: `--help` writes with a panicking `print!`, so a broken pipe turns 0 into 101.
    let status = Command::new(GUARD)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(DeadPipe::new().stdio())
        .stderr(Stdio::null())
        .status()
        .expect("the supervisor must start");

    assert_eq!(status.code(), Some(0));
}

#[cfg(target_os = "macos")]
mod darwin {
    use std::fs::{self, File};
    use std::io::{self, BufRead, BufReader, ErrorKind, PipeReader, PipeWriter, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use mlx_guard_core::{
        Observed, PolicyState, ReportV1, SignalReason, SignalTarget, TerminalKind,
    };

    use super::{DeadPipe, GUARD};

    /// 32 MiB in bytes, the limit of the ramp runs.
    const RAMP_LIMIT_BYTES: u64 = 33_554_432;
    /// The launch notice for `--max-footprint 1TiB`, which is one line and never changes wording.
    const BANNER_FOR_1TIB: &str = "mlx-guard: enforcing a 1099511627776-byte footprint limit; \
         emergency KILL at 1209462790553 bytes, about 10% above the limit";
    const WHAT_HAPPENED: &str = "mlx-guard: what happened: ";
    const WHAT_TO_DO: &str = "mlx-guard: what to do: ";
    /// Every warning-band line but the cleared one starts with this.
    const WARNING: &str = "mlx-guard: warning: ";
    /// 63 MiB and 70 MiB in bytes: the warning threshold and the limit of `--max-footprint 70MiB`,
    /// whose band step is 7 MiB. The allocate fixture holds 64 MiB plus a base measured at about
    /// 1.2 MiB on macOS 27.2, so its footprint sits between the two.
    const BAND_WARNING_BYTES: u64 = 66_060_288;
    const BAND_LIMIT_BYTES: u64 = 73_400_320;
    /// One `echo` per write, two bytes each, and a bound so an abandoned loop still ends by itself.
    const FLOOD: &str = "exec 1>&2; i=0; while [ \"$i\" -lt 2000000 ]; do echo x; i=$((i+1)); done";

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
                "mlx-guard-stderr-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        fn report_path(&self) -> PathBuf {
            self.0.join("report.json")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            // Tolerant: a second panic while a failed test unwinds would abort the whole binary.
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn status_flags(descriptor: &impl AsRawFd) -> i32 {
        // SAFETY: F_GETFL only reads the flags of a descriptor the argument keeps open.
        let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0, "F_GETFL failed: {}", io::Error::last_os_error());
        flags
    }

    fn set_status_flags(descriptor: &impl AsRawFd, flags: i32) {
        // SAFETY: F_SETFL only changes the flags of a descriptor the argument keeps open.
        let result = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFL, flags) };
        assert!(
            result >= 0,
            "F_SETFL failed: {}",
            io::Error::last_os_error()
        );
    }

    /// A pipe filled to the byte before anyone else holds the write end, by a non-blocking fill that
    /// then clears the flag again. No filler thread: a blocked write of the test's own could later
    /// land between a supervisor's bytes, and a thread cannot leave the pipe full to the byte.
    struct FullPipe {
        reader: Option<PipeReader>,
        writer: Option<PipeWriter>,
        filler_bytes: usize,
    }

    impl FullPipe {
        fn new() -> Self {
            let (reader, mut writer) = io::pipe().unwrap();
            let blocking = status_flags(&writer);
            set_status_flags(&writer, blocking | libc::O_NONBLOCK);
            let mut filler_bytes = 0;
            for chunk in [&[b'#'; 4096][..], &[b'#'; 1][..]] {
                loop {
                    match writer.write(chunk) {
                        Ok(written) => filler_bytes += written,
                        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                        Err(error) => panic!("the fill failed: {error}"),
                    }
                }
            }
            set_status_flags(&writer, blocking);
            Self {
                reader: Some(reader),
                writer: Some(writer),
                filler_bytes,
            }
        }

        fn writer_clone(&self) -> PipeWriter {
            self.writer
                .as_ref()
                .expect("the write end is open until drain")
                .try_clone()
                .unwrap()
        }

        fn stdio(&self) -> Stdio {
            Stdio::from(self.writer_clone())
        }

        fn has_nonblocking_set(&self) -> bool {
            let writer = self.writer.as_ref().expect("the write end is still open");
            status_flags(writer) & libc::O_NONBLOCK != 0
        }

        /// Close the read end, so every blocked and later write gets `EPIPE`.
        fn release(&mut self) {
            self.reader = None;
        }

        /// Everything written after the filler, read to end of file. Bounded, so a process that
        /// still holds the write end fails the test instead of hanging it.
        fn drain(&mut self) -> Vec<u8> {
            self.writer = None;
            let mut reader = self.reader.take().expect("drain reads the pipe once");
            set_status_flags(&reader, status_flags(&reader) | libc::O_NONBLOCK);
            let started = Instant::now();
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => bytes.extend_from_slice(&chunk[..read]),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        assert!(
                            started.elapsed() < Duration::from_secs(15),
                            "the pipe did not reach end of file within 15 s"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("the drain failed: {error}"),
                }
            }
            bytes.split_off(self.filler_bytes)
        }
    }

    /// A pseudo-terminal whose master stays open, so the slave never reports hang-up.
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
            Self {
                _master: master,
                slave: Some(slave),
            }
        }

        fn stdin(&mut self) -> Stdio {
            Stdio::from(self.slave.take().expect("the slave is handed out once"))
        }
    }

    /// The pids of `parent`'s direct children; a process that has gone has none.
    fn child_pids(parent: i32) -> Vec<i32> {
        let mut pids = [0_i32; 64];
        let capacity = i32::try_from(std::mem::size_of_val(&pids)).unwrap();
        // SAFETY: the buffer is valid for `capacity` bytes, and the call writes at most that many.
        // It returns a count of pids, not of bytes, and a negative value on failure.
        let count = unsafe {
            libc::proc_listchildpids(parent, pids.as_mut_ptr().cast::<libc::c_void>(), capacity)
        };
        pids.iter()
            .take(usize::try_from(count).unwrap_or(0))
            .copied()
            .filter(|pid| *pid > 0)
            .collect()
    }

    /// A running supervisor that cannot outlive its test. Each command leads its own group, so the
    /// supervisor dying would otherwise leave that group running; every poll records the commands
    /// seen so far, because a supervisor that panicked is no longer there to list them.
    struct Supervisor {
        child: Child,
        commands: Vec<i32>,
    }

    impl Supervisor {
        fn spawn(command: &mut Command) -> Self {
            let child = command.spawn().expect("the supervisor must start");
            let mut supervisor = Self {
                child,
                commands: Vec::new(),
            };
            supervisor.record_commands();
            supervisor
        }

        fn record_commands(&mut self) {
            let pid = i32::try_from(self.child.id()).unwrap();
            for command in child_pids(pid) {
                if !self.commands.contains(&command) {
                    self.commands.push(command);
                }
            }
        }

        fn poll_exit(&mut self) -> Option<ExitStatus> {
            let status = self.child.try_wait().unwrap();
            if status.is_none() {
                self.record_commands();
            }
            status
        }

        /// The exit code once the supervisor has exited; a missed deadline fails the test.
        fn exit_code_within(&mut self, deadline: Duration, context: &str) -> Option<i32> {
            let started = Instant::now();
            loop {
                if let Some(status) = self.poll_exit() {
                    return status.code();
                }
                assert!(
                    started.elapsed() < deadline,
                    "{context}: the supervisor had not exited after {deadline:?}"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }

        /// The validated final report once it exists; a missed deadline fails the test. The report
        /// is replaced atomically, so a file at this path is always the whole report.
        fn report_within(&mut self, path: &Path, deadline: Duration, context: &str) -> ReportV1 {
            let started = Instant::now();
            loop {
                if path.exists() {
                    return read_report(path);
                }
                let state = self.poll_exit();
                assert!(
                    started.elapsed() < deadline,
                    "{context}: no report after {deadline:?}; supervisor status {state:?}"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for Supervisor {
        fn drop(&mut self) {
            if self.child.try_wait().ok().flatten().is_none() {
                self.record_commands();
                self.child.kill().ok();
                self.child.wait().ok();
            }
            for command in &self.commands {
                // Only a pid that still leads a group is one of ours: the commands are started as
                // group leaders, and a pid can be reused once they have gone.
                // SAFETY: getpgid and killpg take plain integers and touch no memory.
                unsafe {
                    if libc::getpgid(*command) == *command {
                        libc::killpg(*command, libc::SIGKILL);
                    }
                }
            }
        }
    }

    /// The test's own flooder, started before the supervisor so its writes overlap the banner.
    struct Flooder(Child);

    impl Drop for Flooder {
        fn drop(&mut self) {
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }

    fn guard() -> Command {
        let mut command = Command::new(GUARD);
        command.stdin(Stdio::null()).stdout(Stdio::null());
        command
    }

    fn read_report(path: &Path) -> ReportV1 {
        ReportV1::from_json(&fs::read_to_string(path).expect("the final report must exist"))
            .expect("the final report must validate")
    }

    /// The fixture lives in another crate of this workspace, so cargo gives this crate no path to
    /// it. `MLX_GUARD_FIXTURE` wins; otherwise it sits next to the supervisor in the target
    /// directory, where a workspace build puts it.
    fn fixture() -> PathBuf {
        let path = std::env::var_os("MLX_GUARD_FIXTURE").map_or_else(
            || Path::new(GUARD).with_file_name("mlx-guard-fixture"),
            PathBuf::from,
        );
        assert!(
            path.is_file(),
            "no fixture at {}; run `cargo build -p mlx-guard-test-support --bin mlx-guard-fixture`",
            path.display()
        );
        path
    }

    /// 64 MiB touched over 500 ms by the fixture, which crosses the 32 MiB limit at about 250 ms
    /// and ends itself after 3 s if nothing stops it.
    fn ramp_run(report_path: &Path, pipe: &FullPipe) -> Command {
        let mut command = guard();
        command
            .args(["run", "--max-footprint", "32MiB", "--sample-interval"])
            .arg("10ms")
            .arg("--report")
            .arg(report_path)
            .arg("--")
            .arg(fixture())
            .args(["ramp", "67108864", "3000"])
            .stderr(pipe.stdio());
        command
    }

    /// `run` with a limit the command never reaches, so only the wall time or its own exit ends it.
    fn roomy_run(report_path: &Path, wall_time: Option<&str>, argv: &[&str]) -> Command {
        let mut command = guard();
        command.args(["run", "--max-footprint", "1TiB"]);
        if let Some(wall_time) = wall_time {
            command.args(["--wall-time", wall_time]);
        }
        command
            .args(["--sample-interval", "10ms", "--report"])
            .arg(report_path)
            .arg("--")
            .args(argv);
        command
    }

    /// `run` over the allocate fixture at a 70 MiB limit, sampled every 50 ms, ended by the wall
    /// time. The fixture allocates once the test writes `allocate` to the piped stdin, and its own
    /// 8 s ceiling outlasts the wall time.
    fn band_run(report_path: &Path, wall_time: &str) -> Command {
        let mut command = guard();
        command
            .args([
                "run",
                "--max-footprint",
                "70MiB",
                "--sample-interval",
                "50ms",
            ])
            .args(["--wall-time", wall_time, "--report"])
            .arg(report_path)
            .arg("--")
            .arg(fixture())
            .args(["allocate", "67108864", "8000"])
            .stdin(Stdio::piped());
        command
    }

    /// Tell the allocate fixture to allocate. The returned stdin stays open for as long as the
    /// caller holds it: the fixture exits on end of file, which would end the run early.
    fn start_allocating(supervisor: &mut Supervisor) -> ChildStdin {
        let mut stdin = supervisor
            .child
            .stdin
            .take()
            .expect("the band run pipes stdin");
        stdin.write_all(b"allocate\n").unwrap();
        stdin
    }

    fn entered_the_band(report: &ReportV1) -> bool {
        report.transitions.iter().any(|transition| {
            transition.from == PolicyState::Normal && transition.to == PolicyState::Warning
        })
    }

    /// Read a stream to its end on a thread, so a stuck writer shows as a timeout in the test.
    fn read_in_background(mut stream: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).ok();
            sender.send(bytes).ok();
        });
        receiver
    }

    #[test]
    fn a_filled_pipe_blocks_a_writer_of_any_size() {
        // Bug: the fill stops short, or the pipe grows again for a larger blocking write, so a
        // supervisor write slips in and every full-pipe test here passes without testing anything.
        for size in [1_usize, 512, 4096] {
            let mut pipe = FullPipe::new();
            let mut writer = pipe.writer_clone();
            let (finished, write_returned) = mpsc::channel();
            let blocked = thread::spawn(move || {
                let result = writer.write(&vec![b'!'; size]);
                finished.send(()).ok();
                result
            });

            assert_eq!(
                write_returned.recv_timeout(Duration::from_millis(500)),
                Err(RecvTimeoutError::Timeout),
                "a {size}-byte blocking write returned on a pipe filled with {} bytes",
                pipe.filler_bytes
            );

            pipe.release();
            let error = blocked.join().unwrap().unwrap_err();
            assert_eq!(error.kind(), ErrorKind::BrokenPipe);
        }
    }

    #[test]
    fn a_full_unread_stderr_pipe_does_not_stop_a_footprint_intervention() {
        // Bug: the banner is written with `eprintln!` on the supervising thread, so the run blocks
        // before sampling, with the command already running and unwatched.
        let directory = TestDirectory::new();
        let report_path = directory.report_path();
        let mut pipe = FullPipe::new();
        let mut supervisor = Supervisor::spawn(&mut ramp_run(&report_path, &pipe));

        // The pipe is not read here: the intervention has to happen with stderr stuck.
        let report = supervisor.report_within(
            &report_path,
            Duration::from_secs(15),
            "stderr full and unread",
        );

        assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
        let first_at_limit = report
            .samples
            .iter()
            .find(|sample| {
                matches!(
                    sample.aggregate_footprint_bytes,
                    Observed::Available { value } if value >= RAMP_LIMIT_BYTES
                )
            })
            .expect("a sample must have reached the limit");
        let first_signal = report
            .signals
            .iter()
            .find(|signal| signal.target == SignalTarget::OwnedProcessGroup)
            .expect("the group must have been signalled");
        assert_eq!(first_signal.reason, Some(SignalReason::Footprint));
        let delay = first_signal
            .at_ms
            .checked_sub(first_at_limit.captured_at_ms)
            .expect("the signal must come after the sample that caused it");
        assert!(
            delay <= 500,
            "the first group signal came {delay} ms after the first sample at the limit"
        );

        pipe.release();
        assert_eq!(
            supervisor.exit_code_within(Duration::from_secs(5), "reader released"),
            Some(75)
        );
    }

    #[test]
    fn the_supervisor_never_sets_nonblocking_on_the_shared_stderr() {
        // Bug: the stall is "fixed" by setting O_NONBLOCK on fd 2 and leaving it set. The flag lives
        // on the open file description, which the command shares, so its own stderr writes start
        // failing with EAGAIN.
        // Bug the report check catches: the run never happens (the supervisor exits at launch, or
        // the command ends before any intervention), and the watch passes with nothing to watch.
        // What the poll cannot catch: a flag set and cleared around a single write. Polled every
        // millisecond, it sees only a flag left set on the shared description.
        let directory = TestDirectory::new();
        let report_path = directory.report_path();
        let pipe = FullPipe::new();
        assert!(
            !pipe.has_nonblocking_set(),
            "the fill must clear its own flag"
        );
        let mut supervisor = Supervisor::spawn(&mut ramp_run(&report_path, &pipe));

        // Watch the whole live run, from the launch notice through the intervention at about
        // 250 ms; the report ends the watch once the run is over. The deadline is the one the other
        // ramp runs here allow for their report.
        let deadline = Duration::from_secs(15);
        let started = Instant::now();
        while started.elapsed() < deadline && !report_path.exists() {
            assert!(
                !pipe.has_nonblocking_set(),
                "O_NONBLOCK appeared on the shared stderr {:?} into the run",
                started.elapsed()
            );
            thread::sleep(Duration::from_millis(1));
        }

        let report = supervisor.report_within(
            &report_path,
            deadline.saturating_sub(started.elapsed()),
            "stderr full and watched for O_NONBLOCK",
        );
        assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
    }

    #[test]
    fn a_dead_stderr_reader_keeps_the_outcome_code_and_the_report() {
        // Bug: the banner panics on EPIPE, so the run exits 101 and writes no report.
        let directory = TestDirectory::new();
        let report_path = directory.report_path();
        let mut supervisor = Supervisor::spawn(
            roomy_run(&report_path, Some("200ms"), &["/bin/sleep", "5"])
                .stderr(DeadPipe::new().stdio()),
        );

        assert_eq!(
            supervisor.exit_code_within(Duration::from_secs(15), "dead reader, wall time"),
            Some(75)
        );
        assert_eq!(
            read_report(&report_path).outcome.kind,
            TerminalKind::PolicyIntervention
        );
    }

    #[test]
    fn a_dead_stderr_reader_keeps_the_commands_exit_code() {
        // Bug: the same panic on the fast-exit path, so the command's 3 becomes 101.
        let directory = TestDirectory::new();
        let report_path = directory.report_path();
        let mut supervisor = Supervisor::spawn(
            roomy_run(&report_path, None, &["/bin/sh", "-c", "exit 3"])
                .stderr(DeadPipe::new().stdio()),
        );

        assert_eq!(
            supervisor.exit_code_within(Duration::from_secs(15), "dead reader, fast exit"),
            Some(3)
        );
        assert_eq!(
            read_report(&report_path).outcome.kind,
            TerminalKind::ChildExited { code: 3 }
        );
    }

    #[test]
    fn a_terminal_stdin_notice_survives_a_dead_reader() {
        // Bug: the stdin notice still uses `eprintln!`, so it panics before the banner is reached.
        let directory = TestDirectory::new();
        let report_path = directory.report_path();
        let mut pty = Pty::open();
        let mut supervisor = Supervisor::spawn(
            roomy_run(&report_path, None, &["/usr/bin/true"])
                .stdin(pty.stdin())
                .stderr(DeadPipe::new().stdio()),
        );

        assert_eq!(
            supervisor.exit_code_within(Duration::from_secs(15), "dead reader, terminal stdin"),
            Some(0)
        );
    }

    #[test]
    fn lines_on_a_fast_exit_stay_ahead_of_the_plain_lines() {
        // Bug: the queue is not finished before the end-of-run lines, so the banner lands after them.
        for repetition in 0..20 {
            let directory = TestDirectory::new();
            let mut supervisor = Supervisor::spawn(
                roomy_run(&directory.report_path(), Some("30ms"), &["/bin/sleep", "5"])
                    .stderr(Stdio::piped()),
            );
            let stderr = read_in_background(supervisor.child.stderr.take().unwrap());

            assert_eq!(
                supervisor.exit_code_within(Duration::from_secs(15), "healthy pipe"),
                Some(75)
            );
            let bytes = stderr
                .recv_timeout(Duration::from_secs(15))
                .expect("stderr must reach end of file");
            let text = String::from_utf8_lossy(&bytes);
            let banner = text.find(BANNER_FOR_1TIB);
            let happened = text.find(WHAT_HAPPENED);
            assert!(
                banner.is_some() && happened.is_some() && banner < happened,
                "repetition {repetition}: the banner must come before the plain lines: {text:?}"
            );
        }
    }

    #[test]
    fn a_full_stderr_pipe_does_not_hold_back_the_stdout_summary() {
        // Bug: the notices are finished before the stdout write, so a stuck stderr holds back the
        // summary line that a caller reading only stdout is waiting for.
        let directory = TestDirectory::new();
        let mut pipe = FullPipe::new();
        let mut supervisor = Supervisor::spawn(
            roomy_run(
                &directory.report_path(),
                Some("200ms"),
                &["/bin/sleep", "5"],
            )
            .stdout(Stdio::piped())
            .stderr(pipe.stdio()),
        );
        let stdout = BufReader::new(supervisor.child.stdout.take().unwrap());
        let (line_sender, first_line) = mpsc::channel();
        thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                line_sender.send(line).ok();
            }
        });

        let summary = first_line
            .recv_timeout(Duration::from_secs(15))
            .expect("the summary line must arrive within 15 s while stderr is full and unread");

        assert!(summary.starts_with("mlx-guard: "), "{summary:?}");
        assert!(
            supervisor.poll_exit().is_none(),
            "the supervisor must still be waiting on stderr to write the end-of-run lines"
        );
        let stderr = pipe.drain();
        assert_eq!(
            supervisor.exit_code_within(Duration::from_secs(5), "stderr drained"),
            Some(75)
        );
        let text = String::from_utf8_lossy(&stderr);
        assert!(
            text.contains(WHAT_HAPPENED) && text.contains(WHAT_TO_DO),
            "{text:?}"
        );
    }

    #[test]
    fn a_full_pipe_does_not_hold_up_a_run_with_no_final_lines() {
        // Bug: the notices are joined unconditionally, so a clean exit waits for a reader nobody
        // promised, with no end-of-run line left to deliver.
        let directory = TestDirectory::new();
        let report_path = directory.report_path();
        let pipe = FullPipe::new();
        let mut supervisor = Supervisor::spawn(
            roomy_run(&report_path, None, &["/bin/sh", "-c", "exit 3"]).stderr(pipe.stdio()),
        );

        assert_eq!(
            supervisor.exit_code_within(Duration::from_secs(5), "stderr full and unread"),
            Some(3)
        );
        assert_eq!(
            read_report(&report_path).outcome.kind,
            TerminalKind::ChildExited { code: 3 }
        );
    }

    /// What a reader saw on a flooded pipe: every line that was not flood, and how much flood came
    /// on either side of the banner.
    struct FloodReading {
        other_lines: Vec<String>,
        flood_before_banner: u64,
        flood_after_banner: u64,
    }

    fn read_flood(reader: PipeReader, flood_started: mpsc::Sender<()>) -> FloodReading {
        let mut reader = BufReader::new(reader);
        let mut reading = FloodReading {
            other_lines: Vec::new(),
            flood_before_banner: 0,
            flood_after_banner: 0,
        };
        let mut flood_started = Some(flood_started);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return reading,
                Ok(_) if line == b"x\n" => {
                    if let Some(started) = flood_started.take() {
                        started.send(()).ok();
                    }
                    if reading
                        .other_lines
                        .iter()
                        .any(|seen| seen.contains("emergency KILL"))
                    {
                        reading.flood_after_banner += 1;
                    } else {
                        reading.flood_before_banner += 1;
                    }
                }
                Ok(_) => reading
                    .other_lines
                    .push(String::from_utf8_lossy(&line).into_owned()),
            }
        }
    }

    #[test]
    fn the_banner_is_not_split_by_the_commands_output() {
        // Bug: the banner goes out in two writes, or is longer than PIPE_BUF, so output from the
        // command or anyone else sharing the pipe lands inside it. Only the banner is checked here:
        // the fit and band lines go out through the same `Notices` path, one write per line, and
        // their size against PIPE_BUF is pinned in mlx-guard-core's `warning_goldens.rs`.
        for repetition in 0..20 {
            let directory = TestDirectory::new();
            let (reader, writer) = io::pipe().unwrap();
            let (started_sender, flood_started) = mpsc::channel();
            let (reading_sender, reading) = mpsc::channel();
            thread::spawn(move || {
                reading_sender.send(read_flood(reader, started_sender)).ok();
            });
            // A command only starts writing after the supervisor's launch, which can be after the
            // banner. This flooder is already writing, so the banner always lands inside a flood.
            let flooder = Flooder(
                Command::new("/bin/sh")
                    .args(["-c", FLOOD])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::from(writer.try_clone().unwrap()))
                    .spawn()
                    .expect("the flooder must start"),
            );
            flood_started
                .recv_timeout(Duration::from_secs(15))
                .expect("the flood must have started");

            let mut supervisor = Supervisor::spawn(
                roomy_run(
                    &directory.report_path(),
                    Some("50ms"),
                    &["/bin/sh", "-c", FLOOD],
                )
                .stderr(Stdio::from(writer.try_clone().unwrap())),
            );
            drop(writer);
            let exit = supervisor.exit_code_within(Duration::from_secs(15), "flooded pipe");
            drop(flooder);
            let reading = reading
                .recv_timeout(Duration::from_secs(15))
                .expect("the pipe must reach end of file");

            assert_eq!(exit, Some(75), "repetition {repetition}");
            assert!(
                reading.flood_before_banner > 0 && reading.flood_after_banner > 0,
                "repetition {repetition}: the banner must land inside the flood, but {} flood \
                 lines came before it and {} after",
                reading.flood_before_banner,
                reading.flood_after_banner
            );
            let banners: Vec<&String> = reading
                .other_lines
                .iter()
                .filter(|line| line.contains("emergency KILL"))
                .collect();
            let expected = format!("{BANNER_FOR_1TIB}\n");
            assert_eq!(
                banners,
                [&expected],
                "repetition {repetition}: the banner must appear whole, once"
            );
        }
    }

    #[test]
    fn a_full_pipe_keeps_the_sample_cadence_inside_the_band() {
        // Bug: the band line is written on the supervising thread, so the loop stalls on the full
        // pipe as soon as the line fires and stops sampling with the command still running.
        let directory = TestDirectory::new();
        let report_path = directory.report_path();
        let mut pipe = FullPipe::new();
        let mut supervisor = Supervisor::spawn(band_run(&report_path, "3s").stderr(pipe.stdio()));
        let _stdin = start_allocating(&mut supervisor);

        // The pipe is not read here: the whole run, band crossing included, happens with it stuck.
        let report = supervisor.report_within(
            &report_path,
            Duration::from_secs(15),
            "stderr full and unread inside the band",
        );

        let footprints: Vec<u64> = report
            .samples
            .iter()
            .filter_map(|sample| match sample.aggregate_footprint_bytes {
                Observed::Available { value } => Some(value),
                _ => None,
            })
            .collect();
        assert!(
            footprints.iter().any(|value| *value >= BAND_WARNING_BYTES)
                && footprints.iter().all(|value| *value < BAND_LIMIT_BYTES),
            "the allocation must sit in the band, short of the limit: {footprints:?}"
        );
        assert!(
            entered_the_band(&report),
            "a normal to warning transition must be recorded: {:?}",
            report.transitions
        );
        assert!(
            report.samples.len() >= 30,
            "a 3 s run sampled every 50 ms kept only {} samples",
            report.samples.len()
        );
        let largest_gap = report
            .samples
            .windows(2)
            .map(|pair| {
                pair[1]
                    .captured_at_ms
                    .saturating_sub(pair[0].captured_at_ms)
            })
            .max()
            .expect("at least two samples");
        assert!(
            largest_gap <= 500,
            "sampling stalled for {largest_gap} ms between two samples"
        );
        let term = report
            .signals
            .iter()
            .find(|signal| signal.signal == 15 && signal.target == SignalTarget::OwnedProcessGroup)
            .expect("the wall time must send TERM to the group");
        assert_eq!(term.reason, Some(SignalReason::WallTime));
        assert!(
            term.at_ms.abs_diff(3000) <= 500,
            "the 3 s wall-time TERM came at {} ms",
            term.at_ms
        );

        pipe.release();
        assert_eq!(
            supervisor.exit_code_within(Duration::from_secs(5), "reader released"),
            Some(75)
        );
    }

    /// The first line on `reader`, read on a thread so a silent pipe fails the test instead of
    /// hanging it, handed back with the reader so the caller decides when it closes.
    fn first_line(reader: PipeReader) -> (String, PipeReader) {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).ok();
            sender
                .send((
                    String::from_utf8_lossy(&line).into_owned(),
                    reader.into_inner(),
                ))
                .ok();
        });
        receiver
            .recv_timeout(Duration::from_secs(15))
            .expect("the first stderr line must arrive within 15 s")
    }

    #[test]
    fn a_reader_that_dies_mid_run_keeps_the_outcome_code_and_the_report() {
        // Bug: the band line's write treats the reader's EPIPE as fatal, or panics, so a reader
        // that was alive at launch and gone by the crossing turns the wall-time 75 into 101 with no
        // report.
        let directory = TestDirectory::new();
        let report_path = directory.report_path();
        let (reader, writer) = io::pipe().unwrap();
        let mut supervisor =
            Supervisor::spawn(band_run(&report_path, "3s").stderr(Stdio::from(writer)));

        let (banner, reader) = first_line(reader);
        assert!(
            banner.contains("emergency KILL"),
            "the reader must be alive at launch and see the banner: {banner:?}"
        );
        drop(reader);
        let _stdin = start_allocating(&mut supervisor);

        assert_eq!(
            supervisor.exit_code_within(Duration::from_secs(15), "reader gone before the band"),
            Some(75)
        );
        let report = read_report(&report_path);
        assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
        assert!(
            entered_the_band(&report),
            "the run must have crossed into the band after the reader went: {:?}",
            report.transitions
        );
    }

    /// The drained stderr of one ramp run, split into newline-ended lines.
    fn lines_of(bytes: &[u8]) -> Vec<String> {
        String::from_utf8_lossy(bytes)
            .split_inclusive('\n')
            .map(str::to_owned)
            .collect()
    }

    /// Check one drained stream: the two plain lines arrived last, contiguous and whole, and a band
    /// line was held back ahead of them. Returns what went wrong, so the caller can name the
    /// repetition. The late-line check runs before the held-back check, so a band line written
    /// after the plain lines is reported as late, not as missing from ahead of them.
    fn end_of_run_lines_last(lines: &[String]) -> Result<(), String> {
        let happened = lines
            .iter()
            .position(|line| line.starts_with(WHAT_HAPPENED))
            .ok_or("no what-happened line")?;
        let to_do = lines
            .get(happened + 1)
            .filter(|line| line.starts_with(WHAT_TO_DO))
            .ok_or("the what-to-do line does not follow the what-happened line directly")?;
        if let Some(late) = lines[happened + 2..]
            .iter()
            .find(|line| line.contains("mlx-guard:"))
        {
            return Err(format!("a line came after the plain lines: {late:?}"));
        }
        if !lines[..happened]
            .iter()
            .any(|line| line.starts_with(WARNING))
        {
            return Err("no warning line was held back ahead of the plain lines".to_owned());
        }
        for line in [&lines[happened], to_do] {
            if !line.ends_with('\n') || line[1..].contains("mlx-guard:") {
                return Err(format!("a plain line is not whole: {line:?}"));
            }
        }
        if !to_do.contains(" More: https://github.com/IonDen/mlx-guard/blob/v") {
            return Err(format!("the what-to-do line lost its link: {to_do:?}"));
        }
        Ok(())
    }

    #[test]
    fn a_reader_that_resumes_gets_the_end_of_run_lines_last() {
        // Bug: a blocked or queued band line is written after, or inside, the plain lines once the
        // reader comes back. The race may not lose every time, so the unit test of `finish` with a
        // stuck write is the sharp check; this one shows what Darwin delivers.
        for repetition in 0..10 {
            let directory = TestDirectory::new();
            let report_path = directory.report_path();
            let mut pipe = FullPipe::new();
            // 64 MiB is reached by the ramp's 64 MiB plus its base, and its 70.4 MiB emergency
            // threshold is not, so the run crosses the band and stops on the limit with TERM.
            let mut supervisor = Supervisor::spawn(
                guard()
                    .args([
                        "run",
                        "--max-footprint",
                        "64MiB",
                        "--sample-interval",
                        "10ms",
                    ])
                    .arg("--report")
                    .arg(&report_path)
                    .arg("--")
                    .arg(fixture())
                    .args(["ramp", "67108864", "3000"])
                    .stderr(pipe.stdio()),
            );

            let report = supervisor.report_within(
                &report_path,
                Duration::from_secs(15),
                "stderr full until the report",
            );
            assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
            thread::sleep(Duration::from_millis(500));
            let lines = lines_of(&pipe.drain());
            assert_eq!(
                supervisor.exit_code_within(Duration::from_secs(5), "stderr drained"),
                Some(75),
                "repetition {repetition}"
            );

            if repetition == 0 {
                // The record of what Darwin delivered, shown with --nocapture or on failure.
                println!("repetition 0 drained stderr: {lines:?}");
            }
            if let Err(problem) = end_of_run_lines_last(&lines) {
                panic!("repetition {repetition}: {problem}: {lines:?}");
            }
        }
    }
}
