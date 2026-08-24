#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, IsTerminal};
use std::num::NonZeroI32;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};

use crate::{CHECKPOINT_FD_ENV, CheckpointWorkerEndpoint, SignalNumber, SignalResult};

const CHECKPOINT_CHILD_FD: libc::c_int = 198;
static TERMINAL_SIGNAL_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// Owned integration descriptor closed once the native runtime can accept client signals.
#[derive(Debug)]
pub struct ClientReady {
    descriptor: Option<libc::c_int>,
}

impl ClientReady {
    /// Take ownership of an optional descriptor supplied by the external Python client.
    ///
    /// The descriptor is marked close-on-exec before any worker is launched. Dropping this value
    /// closes the descriptor, which wakes the client without a write or SIGPIPE risk.
    ///
    /// # Errors
    ///
    /// Returns a redacted control error when the descriptor is not live or cannot be configured.
    pub fn take(raw: Option<i32>) -> Result<Self, ControlError> {
        let Some(raw) = raw else {
            return Ok(Self { descriptor: None });
        };
        if raw < 3 {
            return Err(ControlError::from_io(
                ControlErrorKind::ClientReadyUnavailable,
                io::Error::from_raw_os_error(libc::EBADF),
            ));
        }
        // SAFETY: F_GETFD has no pointer arguments and reports invalid descriptors with EBADF.
        let current = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        if current == -1
            // SAFETY: the validated descriptor and FD_CLOEXEC are valid fcntl arguments.
            || unsafe {
                libc::fcntl(raw, libc::F_SETFD, current | libc::FD_CLOEXEC)
            } == -1
        {
            return Err(ControlError::from_io(
                ControlErrorKind::ClientReadyUnavailable,
                io::Error::last_os_error(),
            ));
        }
        Ok(Self {
            descriptor: Some(raw),
        })
    }

    /// Close the readiness descriptor without writing client-visible data.
    pub fn notify(mut self) {
        self.close();
    }

    fn close(&mut self) {
        if let Some(descriptor) = self.descriptor.take() {
            // SAFETY: validation succeeded and this integration contract transfers the descriptor.
            unsafe {
                libc::close(descriptor);
            }
        }
    }
}

impl Drop for ClientReady {
    fn drop(&mut self) {
        self.close();
    }
}

/// Query the current SIGHUP disposition without changing it (models `nohup` intent).
///
/// # Errors
///
/// Returns `ControlError` when the sigaction query itself fails.
pub fn hangup_is_ignored() -> Result<bool, ControlError> {
    // SAFETY: output is initialized by the successful query below; passing a null new-action
    // pointer makes this a query that does not alter the current disposition.
    let mut current = unsafe { std::mem::zeroed::<libc::sigaction>() };
    // SAFETY: the output pointer is valid for the duration of the call and no new action is
    // installed.
    if unsafe { libc::sigaction(libc::SIGHUP, std::ptr::null(), &raw mut current) } == -1 {
        return Err(ControlError::from_io(
            ControlErrorKind::TerminalSignalMonitorUnavailable,
            io::Error::last_os_error(),
        ));
    }
    Ok(current.sa_sigaction == libc::SIG_IGN)
}

/// Installed nonblocking capture of SIGHUP (when not already ignored), SIGINT, and SIGTERM for one
/// supervisor process.
#[derive(Debug)]
pub struct TerminalSignalMonitor {
    read: OwnedFd,
    _write: OwnedFd,
    previous_hangup: Option<libc::sigaction>,
    previous_interrupt: libc::sigaction,
    previous_terminate: libc::sigaction,
}

impl TerminalSignalMonitor {
    /// Install process-wide handlers that enqueue terminal signals without performing policy work.
    ///
    /// A caller that already disposed SIGHUP to `SIG_IGN` (the `nohup` convention) keeps that
    /// disposition: the monitor probes it via [`hangup_is_ignored`] and skips installing its own
    /// handler for SIGHUP in that case, so it never overrides deliberate `nohup` intent.
    ///
    /// # Errors
    ///
    /// Returns a redacted process-control error if another monitor is active or setup fails.
    pub fn install() -> Result<Self, ControlError> {
        let hangup_ignored = hangup_is_ignored()?;
        let mut descriptors = [-1; 2];
        // SAFETY: `descriptors` points to two writable integers as required by `pipe`.
        if unsafe { libc::pipe(descriptors.as_mut_ptr()) } == -1 {
            return Err(ControlError::from_io(
                ControlErrorKind::TerminalSignalMonitorUnavailable,
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: successful `pipe` returned two new owned descriptors.
        let read = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
        // SAFETY: successful `pipe` returned two new owned descriptors.
        let write = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
        configure_signal_pipe(read.as_raw_fd(), true)?;
        configure_signal_pipe(write.as_raw_fd(), true)?;
        TERMINAL_SIGNAL_WRITE_FD
            .compare_exchange(-1, write.as_raw_fd(), Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                ControlError::new(ControlErrorKind::TerminalSignalMonitorAlreadyInstalled)
            })?;

        // SAFETY: zero is a valid starting representation for `sigaction`; all relevant fields are
        // initialized below before the value is passed to the kernel.
        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        action.sa_sigaction = terminal_signal_handler as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        // SAFETY: `sa_mask` is a live signal set and `sigemptyset` initializes it.
        if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } == -1 {
            TERMINAL_SIGNAL_WRITE_FD.store(-1, Ordering::Release);
            return Err(ControlError::from_io(
                ControlErrorKind::TerminalSignalMonitorUnavailable,
                io::Error::last_os_error(),
            ));
        }
        // SIGHUP is installed only when the caller has not already disposed it to `SIG_IGN`; a
        // `nohup`-launched supervisor keeps that disposition instead of overriding it.
        let previous_hangup = if hangup_ignored {
            None
        } else {
            // SAFETY: output is initialized by the successful call below.
            let mut previous = unsafe { std::mem::zeroed::<libc::sigaction>() };
            // SAFETY: the action and output pointers are valid for the duration of the call.
            if unsafe { libc::sigaction(libc::SIGHUP, &raw const action, &raw mut previous) } == -1
            {
                TERMINAL_SIGNAL_WRITE_FD.store(-1, Ordering::Release);
                return Err(ControlError::from_io(
                    ControlErrorKind::TerminalSignalMonitorUnavailable,
                    io::Error::last_os_error(),
                ));
            }
            Some(previous)
        };
        // SAFETY: output values are fully initialized by successful `sigaction` calls.
        let mut previous_interrupt = unsafe { std::mem::zeroed::<libc::sigaction>() };
        // SAFETY: the action and output pointers are valid for the duration of the call.
        if unsafe { libc::sigaction(libc::SIGINT, &raw const action, &raw mut previous_interrupt) }
            == -1
        {
            if let Some(previous_hangup) = previous_hangup {
                // SAFETY: `previous_hangup` came from the successful installation above.
                unsafe {
                    libc::sigaction(
                        libc::SIGHUP,
                        &raw const previous_hangup,
                        std::ptr::null_mut(),
                    );
                }
            }
            TERMINAL_SIGNAL_WRITE_FD.store(-1, Ordering::Release);
            return Err(ControlError::from_io(
                ControlErrorKind::TerminalSignalMonitorUnavailable,
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: output is initialized by the successful call below.
        let mut previous_terminate = unsafe { std::mem::zeroed::<libc::sigaction>() };
        // SAFETY: the action and output pointers are valid for the duration of the call.
        if unsafe {
            libc::sigaction(
                libc::SIGTERM,
                &raw const action,
                &raw mut previous_terminate,
            )
        } == -1
        {
            // SAFETY: `previous_interrupt` came from the successful installation above.
            unsafe {
                libc::sigaction(
                    libc::SIGINT,
                    &raw const previous_interrupt,
                    std::ptr::null_mut(),
                );
            }
            if let Some(previous_hangup) = previous_hangup {
                // SAFETY: `previous_hangup` came from the successful installation above.
                unsafe {
                    libc::sigaction(
                        libc::SIGHUP,
                        &raw const previous_hangup,
                        std::ptr::null_mut(),
                    );
                }
            }
            TERMINAL_SIGNAL_WRITE_FD.store(-1, Ordering::Release);
            return Err(ControlError::from_io(
                ControlErrorKind::TerminalSignalMonitorUnavailable,
                io::Error::last_os_error(),
            ));
        }

        Ok(Self {
            read,
            _write: write,
            previous_hangup,
            previous_interrupt,
            previous_terminate,
        })
    }

    /// Report whether this monitor skipped installing its own SIGHUP handler because the caller
    /// had already disposed it to `SIG_IGN` before install (the `nohup` convention).
    #[must_use]
    pub const fn hangup_ignored(&self) -> bool {
        self.previous_hangup.is_none()
    }

    /// Drain all currently queued terminal signals in arrival order without blocking.
    ///
    /// # Errors
    ///
    /// Returns a redacted process-control error for an unexpected descriptor read failure.
    pub fn poll(&self) -> Result<Vec<SignalNumber>, ControlError> {
        let mut signals = Vec::new();
        let mut buffer = [0_u8; 32];
        loop {
            // SAFETY: the descriptor is live and `buffer` is writable for its full length.
            let count = unsafe {
                libc::read(
                    self.read.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if count > 0 {
                let count = usize::try_from(count).map_err(|_| {
                    ControlError::new(ControlErrorKind::TerminalSignalMonitorUnavailable)
                })?;
                signals.extend(buffer[..count].iter().filter_map(|value| {
                    matches!(*value, 1 | 2 | 15)
                        .then(|| SignalNumber::new(*value))
                        .flatten()
                }));
                continue;
            }
            if count == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EAGAIN) => break,
                Some(libc::EINTR) => {}
                _ => {
                    return Err(ControlError::from_io(
                        ControlErrorKind::TerminalSignalMonitorUnavailable,
                        error,
                    ));
                }
            }
        }
        Ok(signals)
    }
}

impl Drop for TerminalSignalMonitor {
    fn drop(&mut self) {
        // SAFETY: both values were returned by successful `sigaction` installations for these
        // exact signals. Restoration is best effort during teardown.
        unsafe {
            libc::sigaction(
                libc::SIGINT,
                &raw const self.previous_interrupt,
                std::ptr::null_mut(),
            );
            libc::sigaction(
                libc::SIGTERM,
                &raw const self.previous_terminate,
                std::ptr::null_mut(),
            );
        }
        // SIGHUP is restored only when this monitor actually installed a handler for it. An
        // unconditional restore would write a zeroed `sigaction` (`SIG_DFL`) over a caller's
        // deliberate `SIG_IGN` when the install was skipped.
        if let Some(previous_hangup) = self.previous_hangup {
            // SAFETY: `previous_hangup` came from a successful `sigaction` installation for
            // SIGHUP. Restoration is best effort during teardown.
            unsafe {
                libc::sigaction(
                    libc::SIGHUP,
                    &raw const previous_hangup,
                    std::ptr::null_mut(),
                );
            }
        }
        TERMINAL_SIGNAL_WRITE_FD.store(-1, Ordering::Release);
    }
}

extern "C" fn terminal_signal_handler(signal: libc::c_int) {
    let descriptor = TERMINAL_SIGNAL_WRITE_FD.load(Ordering::Relaxed);
    let Ok(value) = u8::try_from(signal) else {
        return;
    };
    if descriptor >= 0 {
        // SAFETY: `descriptor` remains open until after handler restoration; `write` is
        // async-signal-safe and the nonblocking pipe makes this bounded.
        unsafe {
            libc::write(descriptor, (&raw const value).cast(), 1);
        }
    }
}

fn configure_signal_pipe(descriptor: libc::c_int, nonblocking: bool) -> Result<(), ControlError> {
    // SAFETY: `descriptor` is a live pipe descriptor and `F_GETFD` has no pointer arguments.
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if descriptor_flags == -1
        // SAFETY: the descriptor is live and the requested flags are valid.
        || unsafe { libc::fcntl(descriptor, libc::F_SETFD, descriptor_flags | libc::FD_CLOEXEC) }
            == -1
    {
        return Err(ControlError::from_io(
            ControlErrorKind::TerminalSignalMonitorUnavailable,
            io::Error::last_os_error(),
        ));
    }
    if nonblocking {
        // SAFETY: `descriptor` is live and `F_GETFL` has no pointer arguments.
        let status_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        if status_flags == -1
            // SAFETY: the descriptor is live and O_NONBLOCK is a valid status flag.
            || unsafe { libc::fcntl(descriptor, libc::F_SETFL, status_flags | libc::O_NONBLOCK) }
                == -1
        {
            return Err(ControlError::from_io(
                ControlErrorKind::TerminalSignalMonitorUnavailable,
                io::Error::last_os_error(),
            ));
        }
    }
    Ok(())
}

/// How one child standard stream is connected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdioMode {
    Inherit,
    Null,
    Piped,
}

impl StdioMode {
    fn open(self) -> Stdio {
        match self {
            Self::Inherit => Stdio::inherit(),
            Self::Null => Stdio::null(),
            Self::Piped => Stdio::piped(),
        }
    }
}

/// Fully explicit direct-exec options for one supervised root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchOptions {
    pub command: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub clear_env: bool,
    pub env: BTreeMap<String, String>,
    pub stdin: StdioMode,
    pub stdout: StdioMode,
    pub stderr: StdioMode,
}

/// Stable class of a pre-launch or exec failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchErrorKind {
    EmptyCommand,
    InvalidWorkingDirectory,
    InteractiveTerminalUnsupported,
    NotFound,
    NotExecutable,
    SpawnFailed,
    ProcessGroupValidationFailed,
    InvalidCheckpointChannel,
}

/// Redacted launch failure suitable for CLI outcome mapping.
#[derive(Debug)]
pub struct LaunchError {
    kind: LaunchErrorKind,
    source: Option<io::Error>,
}

impl LaunchError {
    fn new(kind: LaunchErrorKind) -> Self {
        Self { kind, source: None }
    }

    fn from_io(kind: LaunchErrorKind, source: io::Error) -> Self {
        Self {
            kind,
            source: Some(source),
        }
    }

    /// Return the stable failure class without exposing a command or path.
    #[must_use]
    pub const fn kind(&self) -> LaunchErrorKind {
        self.kind
    }
}

impl fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind {
            LaunchErrorKind::EmptyCommand => "command argv is empty",
            LaunchErrorKind::InvalidWorkingDirectory => "child working directory is invalid",
            LaunchErrorKind::InteractiveTerminalUnsupported => {
                "interactive terminal input is unsupported"
            }
            LaunchErrorKind::NotFound => "command was not found",
            LaunchErrorKind::NotExecutable => "command is not executable",
            LaunchErrorKind::SpawnFailed => "command launch failed",
            LaunchErrorKind::ProcessGroupValidationFailed => {
                "new process group could not be validated"
            }
            LaunchErrorKind::InvalidCheckpointChannel => {
                "checkpoint channel configuration is invalid"
            }
        };
        formatter.write_str(message)
    }
}

impl Error for LaunchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// Reject the frozen v0.1 interactive-terminal boundary.
///
/// # Errors
///
/// Returns [`LaunchErrorKind::InteractiveTerminalUnsupported`] when `is_interactive` is true.
pub fn validate_noninteractive_terminal(is_interactive: bool) -> Result<(), LaunchError> {
    if is_interactive {
        return Err(LaunchError::new(
            LaunchErrorKind::InteractiveTerminalUnsupported,
        ));
    }
    Ok(())
}

/// Root status retained independently from descendant cleanup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootOutcome {
    Exited(u8),
    Signaled(SignalNumber),
}

/// Stable class of a process-control failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlErrorKind {
    WaitFailed,
    InvalidRootStatus,
    UnsupportedExternalSignal,
    InvalidCheckpointEndpoint,
    GroupQueryFailed,
    TerminalSignalMonitorUnavailable,
    TerminalSignalMonitorAlreadyInstalled,
    ClientReadyUnavailable,
    SignalFailed,
}

/// Redacted process-control error.
#[derive(Debug)]
pub struct ControlError {
    kind: ControlErrorKind,
    source: Option<io::Error>,
}

impl ControlError {
    fn new(kind: ControlErrorKind) -> Self {
        Self { kind, source: None }
    }

    fn from_io(kind: ControlErrorKind, source: io::Error) -> Self {
        Self {
            kind,
            source: Some(source),
        }
    }

    /// Return the stable failure class.
    #[must_use]
    pub const fn kind(&self) -> ControlErrorKind {
        self.kind
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind {
            ControlErrorKind::WaitFailed => "waiting for the root process failed",
            ControlErrorKind::InvalidRootStatus => "root process returned an invalid Unix status",
            ControlErrorKind::UnsupportedExternalSignal => {
                "only SIGHUP, SIGINT, and SIGTERM can be forwarded as terminal signals"
            }
            ControlErrorKind::InvalidCheckpointEndpoint => {
                "checkpoint endpoint is not a live member of the owned process group"
            }
            ControlErrorKind::GroupQueryFailed => "owned process-group query failed",
            ControlErrorKind::TerminalSignalMonitorUnavailable => {
                "terminal signal monitoring failed"
            }
            ControlErrorKind::TerminalSignalMonitorAlreadyInstalled => {
                "a terminal signal monitor is already installed"
            }
            ControlErrorKind::ClientReadyUnavailable => {
                "client readiness descriptor configuration failed"
            }
            ControlErrorKind::SignalFailed => "signal delivery failed",
        };
        formatter.write_str(message)
    }
}

impl Error for ControlError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointEndpoint {
    pid: NonZeroI32,
    process_group: NonZeroI32,
}

/// Copyable authority for signalling and querying one validated owned process group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessControlHandle {
    process_group: NonZeroI32,
}

impl ProcessControlHandle {
    /// Check whether the validated owned process group still has a live member without waiting.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when the zero-signal process-group query fails unexpectedly.
    pub fn owned_group_exists(self) -> Result<bool, ControlError> {
        let target = self
            .process_group
            .get()
            .checked_neg()
            .ok_or_else(|| ControlError::new(ControlErrorKind::GroupQueryFailed))?;
        // SAFETY: the process group is positive and validated, so its negation cannot target zero.
        if unsafe { libc::kill(target, 0) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(ControlError::from_io(
                ControlErrorKind::GroupQueryFailed,
                error,
            )),
        }
    }

    /// Forward one supported terminal signal unchanged to the owned group.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] for unsupported job-control signals or a failed system call.
    pub fn forward_terminal_signal(
        self,
        signal: SignalNumber,
    ) -> Result<SignalResult, ControlError> {
        if !matches!(signal.get(), 1 | 2 | 15) {
            return Err(ControlError::new(
                ControlErrorKind::UnsupportedExternalSignal,
            ));
        }
        self.signal_group(signal)
    }

    /// Send SIGTERM to the validated owned process group.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when signal delivery fails unexpectedly.
    pub fn terminate_group(self) -> Result<SignalResult, ControlError> {
        self.signal_group(signal_number(libc::SIGTERM))
    }

    /// Send SIGKILL to the validated owned process group.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when signal delivery fails unexpectedly.
    pub fn kill_group(self) -> Result<SignalResult, ControlError> {
        self.signal_group(signal_number(libc::SIGKILL))
    }

    /// Send a checkpoint request only to its validated cooperative endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] if the endpoint no longer belongs to this group or delivery fails.
    pub fn signal_checkpoint(
        self,
        endpoint: &CheckpointEndpoint,
        signal: SignalNumber,
    ) -> Result<SignalResult, ControlError> {
        if endpoint.process_group != self.process_group {
            return Err(ControlError::new(
                ControlErrorKind::InvalidCheckpointEndpoint,
            ));
        }
        // SAFETY: the endpoint PID is positive and validated; this recheck narrows PID-reuse risk.
        let observed_group = unsafe { libc::getpgid(endpoint.pid.get()) };
        if observed_group != self.process_group.get() {
            return Err(ControlError::new(
                ControlErrorKind::InvalidCheckpointEndpoint,
            ));
        }
        signal_raw(endpoint.pid.get(), signal)
    }

    fn signal_group(self, signal: SignalNumber) -> Result<SignalResult, ControlError> {
        let target = self
            .process_group
            .get()
            .checked_neg()
            .ok_or_else(|| ControlError::new(ControlErrorKind::SignalFailed))?;
        signal_raw(target, signal)
    }
}

/// A directly launched root plus its validated owned Unix process group.
#[derive(Debug)]
pub struct OwnedProcess {
    child: Child,
    root_pid: NonZeroI32,
    process_group: NonZeroI32,
    root_outcome: Option<RootOutcome>,
    cleanup_on_drop: bool,
}

impl OwnedProcess {
    /// Launch literal argv into a new process group without a shell.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError`] for invalid preflight state, failed exec, or group validation.
    pub fn launch(options: &LaunchOptions) -> Result<Self, LaunchError> {
        Self::launch_inner(options, None)
    }

    /// Launch with one explicitly inherited checkpoint descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError`] for normal launch failures or a reserved-environment collision.
    pub fn launch_with_checkpoint(
        options: &LaunchOptions,
        endpoint: CheckpointWorkerEndpoint,
    ) -> Result<Self, LaunchError> {
        let result = Self::launch_inner(options, Some(&endpoint));
        // The supervisor must not retain the worker's side of the socketpair after spawn.
        // Closing it here makes worker exit observable as EOF on the supervisor endpoint.
        drop(endpoint);
        result
    }

    fn launch_inner(
        options: &LaunchOptions,
        checkpoint: Option<&CheckpointWorkerEndpoint>,
    ) -> Result<Self, LaunchError> {
        if options.stdin == StdioMode::Inherit {
            validate_noninteractive_terminal(io::stdin().is_terminal())?;
        }
        let executable = options
            .command
            .first()
            .ok_or_else(|| LaunchError::new(LaunchErrorKind::EmptyCommand))?;
        if executable.is_empty() {
            return Err(LaunchError::new(LaunchErrorKind::EmptyCommand));
        }
        if options.cwd.as_ref().is_some_and(|path| !path.is_dir()) {
            return Err(LaunchError::new(LaunchErrorKind::InvalidWorkingDirectory));
        }

        let mut command = Command::new(executable);
        command.args(&options.command[1..]);
        if let Some(cwd) = &options.cwd {
            command.current_dir(cwd);
        }
        if options.clear_env {
            command.env_clear();
        }
        command.envs(&options.env);
        command
            .stdin(options.stdin.open())
            .stdout(options.stdout.open())
            .stderr(options.stderr.open())
            .process_group(0);
        if let Some(checkpoint) = checkpoint {
            if options.env.contains_key(CHECKPOINT_FD_ENV) {
                return Err(LaunchError::new(LaunchErrorKind::InvalidCheckpointChannel));
            }
            let checkpoint_fd = checkpoint.raw_fd();
            command.env(CHECKPOINT_FD_ENV, CHECKPOINT_CHILD_FD.to_string());
            // SAFETY: this runs after fork and before exec. It duplicates the owned socket onto a
            // reserved descriptor after stdio remapping, then clears close-on-exec on that copy.
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(checkpoint_fd, CHECKPOINT_CHILD_FD) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    let flags = libc::fcntl(CHECKPOINT_CHILD_FD, libc::F_GETFD);
                    if flags == -1
                        || libc::fcntl(
                            CHECKPOINT_CHILD_FD,
                            libc::F_SETFD,
                            flags & !libc::FD_CLOEXEC,
                        ) == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        let mut child = command.spawn().map_err(map_spawn_error)?;
        let raw_pid = i32::try_from(child.id())
            .map_err(|_| LaunchError::new(LaunchErrorKind::ProcessGroupValidationFailed))?;
        let root_pid = NonZeroI32::new(raw_pid)
            .ok_or_else(|| LaunchError::new(LaunchErrorKind::ProcessGroupValidationFailed))?;
        let mut root_outcome = None;

        // SAFETY: `raw_pid` is positive and came from the child returned by `Command::spawn`.
        let observed_group = unsafe { libc::getpgid(raw_pid) };
        if observed_group != raw_pid {
            let launch_race_finished = if observed_group == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                child
                    .try_wait()
                    .map_err(|error| {
                        LaunchError::from_io(LaunchErrorKind::ProcessGroupValidationFailed, error)
                    })?
                    .map(root_outcome_from_status)
                    .transpose()
                    .map_err(|()| LaunchError::new(LaunchErrorKind::ProcessGroupValidationFailed))?
            } else {
                None
            };
            if launch_race_finished.is_none() {
                child.kill().ok();
                child.wait().ok();
                return Err(LaunchError::new(
                    LaunchErrorKind::ProcessGroupValidationFailed,
                ));
            }
            root_outcome = launch_race_finished;
        }

        Ok(Self {
            child,
            root_pid,
            process_group: root_pid,
            root_outcome,
            cleanup_on_drop: true,
        })
    }

    /// Return the root PID as reported by the operating system.
    #[must_use]
    pub const fn root_pid(&self) -> u32 {
        self.root_pid.get().cast_unsigned()
    }

    /// Return the positive, validated process-group ID.
    #[must_use]
    pub const fn process_group_id(&self) -> i32 {
        self.process_group.get()
    }

    /// Return copyable authority for this validated process group without transferring child wait
    /// ownership.
    #[must_use]
    pub const fn control_handle(&self) -> ProcessControlHandle {
        ProcessControlHandle {
            process_group: self.process_group,
        }
    }

    /// Relinquish cleanup ownership without signaling or waiting for the process group.
    ///
    /// This is reserved for observe-only supervision failures, where the supervisor must stop but
    /// is not authorized to intervene in the command.
    pub fn relinquish(mut self) {
        self.cleanup_on_drop = false;
    }

    /// Check whether the validated owned process group still has a live member without waiting.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when the zero-signal process-group query fails unexpectedly.
    pub fn owned_group_exists(&self) -> Result<bool, ControlError> {
        self.control_handle().owned_group_exists()
    }

    /// Take the piped child stdin, if configured.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    /// Take the piped child stdout, if configured.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// Take the piped child stderr, if configured.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Poll only the supervised root, retaining its first terminal status.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when wait fails or the Unix status is not representable.
    pub fn try_wait_root(&mut self) -> Result<Option<RootOutcome>, ControlError> {
        if self.root_outcome.is_some() {
            return Ok(self.root_outcome);
        }
        let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| ControlError::from_io(ControlErrorKind::WaitFailed, error))?
        else {
            return Ok(None);
        };
        let outcome = root_outcome_from_status(status)
            .map_err(|()| ControlError::new(ControlErrorKind::InvalidRootStatus))?;
        self.root_outcome = Some(outcome);
        Ok(self.root_outcome)
    }

    /// Wait only for the supervised root, retaining descendant cleanup ownership.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when wait fails or the Unix status is not representable.
    pub fn wait_root(&mut self) -> Result<RootOutcome, ControlError> {
        if let Some(outcome) = self.root_outcome {
            return Ok(outcome);
        }
        let status = self
            .child
            .wait()
            .map_err(|error| ControlError::from_io(ControlErrorKind::WaitFailed, error))?;
        let outcome = root_outcome_from_status(status)
            .map_err(|()| ControlError::new(ControlErrorKind::InvalidRootStatus))?;
        self.root_outcome = Some(outcome);
        Ok(outcome)
    }

    /// Forward the first supported terminal signal unchanged to the owned group.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] for unsupported job-control signals or a failed system call.
    pub fn forward_terminal_signal(
        &self,
        signal: SignalNumber,
    ) -> Result<SignalResult, ControlError> {
        self.control_handle().forward_terminal_signal(signal)
    }

    /// Send SIGTERM to the validated owned process group.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when signal delivery fails unexpectedly.
    pub fn terminate_group(&self) -> Result<SignalResult, ControlError> {
        self.control_handle().terminate_group()
    }

    /// Send SIGKILL to the validated owned process group.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when signal delivery fails unexpectedly.
    pub fn kill_group(&self) -> Result<SignalResult, ControlError> {
        self.control_handle().kill_group()
    }

    /// Validate a negotiated checkpoint endpoint as a live member of this group.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when the PID is zero, invalid, gone, or outside the owned group.
    pub fn negotiate_checkpoint_endpoint(
        &self,
        pid: u32,
    ) -> Result<CheckpointEndpoint, ControlError> {
        let raw_pid = i32::try_from(pid)
            .ok()
            .and_then(NonZeroI32::new)
            .ok_or_else(|| ControlError::new(ControlErrorKind::InvalidCheckpointEndpoint))?;
        // SAFETY: `raw_pid` is a validated positive PID and no pointers are involved.
        let observed_group = unsafe { libc::getpgid(raw_pid.get()) };
        if observed_group != self.process_group.get() {
            return Err(ControlError::new(
                ControlErrorKind::InvalidCheckpointEndpoint,
            ));
        }
        Ok(CheckpointEndpoint {
            pid: raw_pid,
            process_group: self.process_group,
        })
    }

    /// Send a checkpoint request only to its validated cooperative endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] if the endpoint no longer belongs to this group or delivery fails.
    pub fn signal_checkpoint(
        &self,
        endpoint: &CheckpointEndpoint,
        signal: SignalNumber,
    ) -> Result<SignalResult, ControlError> {
        self.control_handle().signal_checkpoint(endpoint, signal)
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if !self.cleanup_on_drop {
            return;
        }
        self.kill_group().ok();
        self.child.wait().ok();
    }
}

fn map_spawn_error(error: io::Error) -> LaunchError {
    let kind = match error.kind() {
        io::ErrorKind::NotFound => LaunchErrorKind::NotFound,
        io::ErrorKind::PermissionDenied => LaunchErrorKind::NotExecutable,
        _ => LaunchErrorKind::SpawnFailed,
    };
    LaunchError::from_io(kind, error)
}

fn root_outcome_from_status(status: ExitStatus) -> Result<RootOutcome, ()> {
    if let Some(code) = status.code() {
        return u8::try_from(code).map(RootOutcome::Exited).map_err(|_| ());
    }
    let signal = status
        .signal()
        .and_then(|value| u8::try_from(value).ok())
        .and_then(SignalNumber::new)
        .ok_or(())?;
    Ok(RootOutcome::Signaled(signal))
}

fn signal_number(value: i32) -> SignalNumber {
    let value = u8::try_from(value).expect("POSIX signal constants fit u8");
    SignalNumber::new(value).expect("POSIX signal constants are nonzero")
}

/// Return the platform's validated SIGUSR1 value for checkpoint configuration.
#[must_use]
pub fn checkpoint_signal_usr1() -> SignalNumber {
    signal_number(libc::SIGUSR1)
}

fn signal_raw(target: i32, signal: SignalNumber) -> Result<SignalResult, ControlError> {
    if target == 0 {
        return Err(ControlError::new(ControlErrorKind::SignalFailed));
    }
    // SAFETY: `target` is either a validated positive endpoint or negative validated PGID.
    if unsafe { libc::kill(target, i32::from(signal.get())) } == 0 {
        return Ok(SignalResult::Delivered);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(SignalResult::ProcessMissing),
        Some(libc::EPERM) => Ok(SignalResult::PermissionDenied),
        _ => Err(ControlError::from_io(ControlErrorKind::SignalFailed, error)),
    }
}
