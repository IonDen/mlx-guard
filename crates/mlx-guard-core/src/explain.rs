//! Plain outcome lines: one sentence on what happened and one on what to do next.
//!
//! Every sentence is built from report facts, or from the typed cause the supervisor holds when
//! no report exists. The module is pure: no I/O, no floats, and integer arithmetic only, so the
//! Python mirror renders the same bytes from the same report.

#[cfg(unix)]
use crate::StorageErrorKind;
use crate::{
    CheckpointStatus, ObservationError, Observed, PolicyState, ReportMode, ReportV1, SignalReason,
    SignalRecord, SignalResult, SignalTarget, SupervisorOutcome, TerminalKind,
};

const SIGKILL: u8 = 9;
const SIGTERM: u8 = 15;

/// Exit codes mlx-guard also uses, so a command's own exit with one of them is explained.
const COLLISION_CODES: [u8; 6] = [64, 70, 74, 75, 126, 127];

const ANCHOR_COMMAND_LINE: &str = "64-the-command-line";
const ANCHOR_LAUNCH_SETTING: &str = "64-a-launch-setting";
const ANCHOR_BEFORE_LAUNCH_70: &str = "70-before-launch";
const ANCHOR_WHILE_STARTING: &str = "70-while-starting-the-command";
const ANCHOR_READINGS_LOST: &str = "70-footprint-readings-lost";
const ANCHOR_NOT_DELIVERED: &str = "70-a-signal-not-delivered";
const ANCHOR_INTERNAL: &str = "70-an-internal-failure";
const ANCHOR_PATH_USED: &str = "74-report-path-already-used";
const ANCHOR_DIRECTORY: &str = "74-report-directory";
const ANCHOR_BEFORE_LAUNCH_74: &str = "74-before-launch";
const ANCHOR_AFTER_LAUNCH: &str = "74-after-launch";
const ANCHOR_FOOTPRINT: &str = "75-the-footprint-limit";
const ANCHOR_EMERGENCY: &str = "75-emergency-kill";
const ANCHOR_WALL: &str = "75-the-wall-time-limit";
const ANCHOR_PARENT: &str = "75-the-launching-process-exited";
const ANCHOR_IGNORED_SIGNAL: &str = "75-a-signal-the-command-ignored";
const ANCHOR_SURVIVORS: &str = "75-processes-left-behind";
const ANCHOR_UNRECORDED: &str = "75-cause-not-recorded";
const ANCHOR_NOT_EXECUTABLE: &str = "126-not-executable";
const ANCHOR_NOT_FOUND: &str = "127-not-found";
const ANCHOR_PASSED_ON: &str = "signal-passed-on-by-mlx-guard";
const ANCHOR_ELSEWHERE: &str = "signal-from-somewhere-else";
const ANCHOR_COLLISION: &str = "a-code-mlx-guard-also-uses";

/// Troubleshooting-page anchors, in matrix order (shared with `anchors.txt`).
pub const ANCHORS: &[&str] = &[
    ANCHOR_COMMAND_LINE,
    ANCHOR_LAUNCH_SETTING,
    ANCHOR_BEFORE_LAUNCH_70,
    ANCHOR_WHILE_STARTING,
    ANCHOR_READINGS_LOST,
    ANCHOR_NOT_DELIVERED,
    ANCHOR_INTERNAL,
    ANCHOR_PATH_USED,
    ANCHOR_DIRECTORY,
    ANCHOR_BEFORE_LAUNCH_74,
    ANCHOR_AFTER_LAUNCH,
    ANCHOR_FOOTPRINT,
    ANCHOR_EMERGENCY,
    ANCHOR_WALL,
    ANCHOR_PARENT,
    ANCHOR_IGNORED_SIGNAL,
    ANCHOR_SURVIVORS,
    ANCHOR_UNRECORDED,
    ANCHOR_NOT_EXECUTABLE,
    ANCHOR_NOT_FOUND,
    ANCHOR_PASSED_ON,
    ANCHOR_ELSEWHERE,
    ANCHOR_COLLISION,
];

/// One matrix row before the link version is attached.
struct Lines {
    happened: String,
    to_do: String,
    anchor: &'static str,
}

impl Lines {
    fn new(happened: impl Into<String>, to_do: impl Into<String>, anchor: &'static str) -> Self {
        Self {
            happened: happened.into(),
            to_do: to_do.into(),
            anchor,
        }
    }

    fn with_version(self, version: &str) -> PlainOutcome {
        PlainOutcome {
            what_happened: self.happened,
            what_to_do: self.to_do,
            anchor: self.anchor,
            version: version.to_owned(),
        }
    }
}

/// The two plain lines for one outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlainOutcome {
    pub what_happened: String,
    pub what_to_do: String,
    pub anchor: &'static str,
    pub version: String,
}

impl PlainOutcome {
    /// Both lines with the `mlx-guard: ` prefix, each newline-ended, the link on line two.
    #[must_use]
    pub fn stderr_lines(&self) -> String {
        let mut lines = String::new();
        for line in self.text().split('\n') {
            lines.push_str("mlx-guard: ");
            lines.push_str(line);
            lines.push('\n');
        }
        lines
    }

    /// Both lines without the prefix, joined by a newline, no trailing newline.
    #[must_use]
    pub fn text(&self) -> String {
        format!(
            "what happened: {}\nwhat to do: {} More: {}",
            self.what_happened,
            self.what_to_do,
            doc_url(&self.version, self.anchor)
        )
    }
}

/// Phase in which a report-less storage failure happened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoragePhase {
    BeforeLaunch,
    LaunchFailed,
    /// After the launch, before the run finished starting.
    StartUnfinished,
    AfterLaunch,
}

/// Platform-neutral mirror of the persistence error kinds, plus `Unknown`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageClass {
    InvalidPath,
    UnsafeDirectory,
    JournalExists,
    CreateFailed,
    WriteFailed,
    SyncFailed,
    ReadFailed,
    InvalidRecord,
    UnsafeTarget,
    FinalizeFailed,
    Unknown,
}

#[cfg(unix)]
impl From<StorageErrorKind> for StorageClass {
    fn from(kind: StorageErrorKind) -> Self {
        match kind {
            StorageErrorKind::InvalidPath => Self::InvalidPath,
            StorageErrorKind::UnsafeDirectory => Self::UnsafeDirectory,
            StorageErrorKind::JournalExists => Self::JournalExists,
            StorageErrorKind::CreateFailed => Self::CreateFailed,
            StorageErrorKind::WriteFailed => Self::WriteFailed,
            StorageErrorKind::SyncFailed => Self::SyncFailed,
            StorageErrorKind::ReadFailed => Self::ReadFailed,
            StorageErrorKind::InvalidRecord => Self::InvalidRecord,
            StorageErrorKind::UnsafeTarget => Self::UnsafeTarget,
            StorageErrorKind::FinalizeFailed => Self::FinalizeFailed,
        }
    }
}

/// The typed cause of an outcome that has no report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EarlyCause {
    CommandLine {
        flag: Option<&'static str>,
    },
    ClientReadyDescriptor,
    Configuration,
    UnsupportedPlatform,
    FootprintUnavailable,
    RunIdentity,
    SupervisorSetup,
    Storage {
        kind: StorageClass,
        phase: StoragePhase,
    },
}

/// Explain a final report, or return `None` when the outcome needs no plain lines.
///
/// The link version is the report's own `package_version`.
#[must_use]
pub fn explain_report(report: &ReportV1) -> Option<PlainOutcome> {
    let lines = match report.outcome.kind {
        TerminalKind::ChildExited { code } => child_exited(code)?,
        TerminalKind::ChildSignaled { signal } => child_signaled(report, signal),
        TerminalKind::InvalidConfiguration => launch_setting(),
        TerminalKind::LaunchNotExecutable => not_executable(),
        TerminalKind::LaunchNotFound => not_found(),
        TerminalKind::SupervisorFailure => supervisor_failure(report),
        TerminalKind::PolicyIntervention => policy_intervention(report),
        TerminalKind::PartialArtifactFailure => after_launch(None),
    };
    Some(lines.with_version(&report.package_version))
}

/// Explain an outcome that has no report. The link version is this binary's [`crate::VERSION`].
#[must_use]
pub fn explain_early(cause: EarlyCause) -> PlainOutcome {
    let lines = match cause {
        EarlyCause::CommandLine { flag } => command_line(flag),
        EarlyCause::ClientReadyDescriptor => client_ready(),
        EarlyCause::Configuration => configuration(),
        EarlyCause::UnsupportedPlatform => unsupported_platform(),
        EarlyCause::FootprintUnavailable => footprint_unavailable(),
        EarlyCause::RunIdentity => run_identity(),
        EarlyCause::SupervisorSetup => supervisor_setup(),
        EarlyCause::Storage { kind, phase } => storage(kind, phase),
    };
    lines.with_version(crate::VERSION)
}

/// The generic early cause for a supervisor outcome that reached no more specific one.
#[must_use]
pub fn generic_early_cause(outcome: SupervisorOutcome) -> Option<EarlyCause> {
    match outcome {
        SupervisorOutcome::InvalidConfiguration => Some(EarlyCause::Configuration),
        SupervisorOutcome::SupervisorFailure => Some(EarlyCause::SupervisorSetup),
        SupervisorOutcome::PartialArtifactFailure => Some(EarlyCause::Storage {
            kind: StorageClass::Unknown,
            phase: StoragePhase::BeforeLaunch,
        }),
        SupervisorOutcome::ChildExited(_)
        | SupervisorOutcome::ChildSignaled(_)
        | SupervisorOutcome::LaunchNotFound
        | SupervisorOutcome::LaunchNotExecutable
        | SupervisorOutcome::PolicyIntervention => None,
    }
}

/// Render a byte count with binary units, integer arithmetic only.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [(u128, &str); 3] = [(1 << 10, "KiB"), (1 << 20, "MiB"), (1 << 30, "GiB")];
    let value = u128::from(bytes);
    let Some(mut index) = UNITS.iter().rposition(|&(unit, _)| value >= unit) else {
        return format!("{bytes} B");
    };
    let hundredths = |unit: u128| (value * 100 + unit / 2) / unit;
    let mut rendered = hundredths(UNITS[index].0);
    if rendered >= 102_400 && index + 1 < UNITS.len() {
        index += 1;
        rendered = hundredths(UNITS[index].0);
    }
    format!(
        "{}.{:02} {}",
        rendered / 100,
        rendered % 100,
        UNITS[index].1
    )
}

/// Render a duration in milliseconds, integer arithmetic only.
#[must_use]
pub fn format_duration_ms(ms: u64) -> String {
    let ms = u128::from(ms);
    if ms < 1000 {
        return format!("{ms} ms");
    }
    let tenths = (ms + 50) / 100;
    if tenths < 600 {
        return format!("{}.{} s", tenths / 10, tenths % 10);
    }
    let seconds = (ms + 500) / 1000;
    if seconds < 3600 {
        return match seconds % 60 {
            0 => format!("{} min", seconds / 60),
            rest => format!("{} min {rest} s", seconds / 60),
        };
    }
    let minutes = (ms + 30_000) / 60_000;
    match minutes % 60 {
        0 => format!("{} h", minutes / 60),
        rest => format!("{} h {rest} min", minutes / 60),
    }
}

/// Name a signal from the fixed (Darwin-numbered) table, or `signal {n}`.
#[must_use]
pub fn signal_name(signal: u8) -> String {
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        SIGKILL => "SIGKILL",
        10 => "SIGBUS",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        SIGTERM => "SIGTERM",
        30 => "SIGUSR1",
        other => return format!("signal {other}"),
    };
    name.to_owned()
}

/// The troubleshooting link for one release tag and anchor.
#[must_use]
pub fn doc_url(version: &str, anchor: &str) -> String {
    format!("https://github.com/IonDen/mlx-guard/blob/v{version}/docs/TROUBLESHOOTING.md#{anchor}")
}

// ---- Shared rendering helpers ----

/// Render values a sentence compares; if two differ but render alike, print all as bytes.
fn compared_bytes<const N: usize>(values: [u64; N]) -> [String; N] {
    let rendered = values.map(format_bytes);
    let ambiguous = (0..N).any(|left| {
        (left + 1..N)
            .any(|right| values[left] != values[right] && rendered[left] == rendered[right])
    });
    if ambiguous {
        values.map(|value| format!("{value} bytes"))
    } else {
        rendered
    }
}

fn is_group_signal(record: &SignalRecord, signal: u8) -> bool {
    record.target == SignalTarget::OwnedProcessGroup && record.signal == signal
}

fn first_group_signal(report: &ReportV1, signal: u8) -> Option<&SignalRecord> {
    report
        .signals
        .iter()
        .find(|record| is_group_signal(record, signal))
}

fn sent_to_group(signal: u8, at_ms: u64) -> String {
    format!(
        "sent {} to the command's process group at {}",
        signal_name(signal),
        format_duration_ms(at_ms)
    )
}

/// What mlx-guard did, from `signals[]` in order; a record counts as sent only when delivered.
fn action_clause(report: &ReportV1) -> String {
    let signals = &report.signals;
    let term = signals
        .iter()
        .position(|record| is_group_signal(record, SIGTERM))
        .map(|index| (index, &signals[index]));
    let requested = signals.iter().find(|record| {
        record.target == SignalTarget::CooperativeEndpoint
            && record.result == SignalResult::Delivered
    });
    if let Some(request) = requested {
        let asked = format!(
            "asked the command to save a checkpoint at {}",
            format_duration_ms(request.at_ms)
        );
        return match term {
            Some((index, record)) if record.result == SignalResult::Delivered => format!(
                "{asked} and then sent SIGTERM to its process group{}",
                kill_suffix(signals, index)
            ),
            Some((index, record)) if record.result == SignalResult::ProcessMissing => format!(
                "{asked}; the command ended before the SIGTERM that followed could reach it{}",
                kill_suffix(signals, index)
            ),
            _ => asked,
        };
    }
    match term {
        Some((index, record)) if record.result == SignalResult::Delivered => {
            return format!(
                "{}{}",
                sent_to_group(SIGTERM, record.at_ms),
                kill_suffix(signals, index)
            );
        }
        Some((index, record)) if record.result == SignalResult::ProcessMissing => {
            return format!(
                "found the command already ended when it sent SIGTERM at {}{}",
                format_duration_ms(record.at_ms),
                kill_suffix(signals, index)
            );
        }
        _ => {}
    }
    if let Some(kill) = signals
        .iter()
        .find(|record| is_group_signal(record, SIGKILL) && record.result == SignalResult::Delivered)
    {
        return sent_to_group(SIGKILL, kill.at_ms);
    }
    format!(
        "stopped the command at {}",
        format_duration_ms(report.outcome.at_ms)
    )
}

/// The escalation suffix after the first TERM: only a delivered group KILL that follows it. A
/// KILL for a forwarded signal is the second terminal signal, even when the TERM was forwarded too;
/// after a TERM that found the command ended, that KILL is told as a clause of its own.
fn kill_suffix(signals: &[SignalRecord], term_index: usize) -> String {
    let term = &signals[term_index];
    let Some(kill) = signals[term_index + 1..].iter().find(|record| {
        is_group_signal(record, SIGKILL) && record.result == SignalResult::Delivered
    }) else {
        return String::new();
    };
    let gap = format_duration_ms(kill.at_ms.saturating_sub(term.at_ms));
    let second_signal = kill.reason == Some(SignalReason::ExternalSignal);
    if second_signal && term.result == SignalResult::ProcessMissing {
        format!("; a SIGKILL for a second terminal signal reached its process group {gap} later")
    } else if second_signal {
        format!(", and SIGKILL {gap} later, after a second terminal signal arrived")
    } else if kill.reason == term.reason {
        format!(", and SIGKILL {gap} later because it was still running")
    } else {
        String::new()
    }
}

fn checkpoint_clause(report: &ReportV1) -> String {
    let checkpoint = &report.checkpoint;
    match (checkpoint.status, checkpoint.request_id) {
        (CheckpointStatus::AcknowledgedUnverifiedDurability, Some(id)) => {
            format!(" The command acknowledged checkpoint request {id}.")
        }
        (CheckpointStatus::TimedOut, Some(id)) => report
            .configuration
            .checkpoint_timeout_ms
            .map_or_else(String::new, |timeout| {
                format!(
                    " The command did not acknowledge checkpoint request {id} within {}; it may \
                     still have saved it.",
                    format_duration_ms(timeout)
                )
            }),
        (CheckpointStatus::RequestedUnverified, Some(id)) => {
            format!(" The command had not acknowledged checkpoint request {id} when it ended.")
        }
        (CheckpointStatus::RequestedUnverified, None) => {
            " A checkpoint was requested but not acknowledged.".to_owned()
        }
        (CheckpointStatus::Cancelled, _) => {
            " The checkpoint request could not be delivered.".to_owned()
        }
        _ => String::new(),
    }
}

fn resume_clause(report: &ReportV1) -> String {
    match (report.checkpoint.status, report.checkpoint.request_id) {
        (CheckpointStatus::AcknowledgedUnverifiedDurability, Some(id)) => {
            format!("; resume from the state saved for checkpoint request {id}")
        }
        (CheckpointStatus::TimedOut, Some(id)) => {
            format!("; if the command saved state for checkpoint request {id}, resume from it")
        }
        _ => String::new(),
    }
}

// ---- Report-backed rows, in rule order (spec 5.1) ----

fn child_exited(code: u8) -> Option<Lines> {
    COLLISION_CODES.contains(&code).then(|| {
        Lines::new(
            format!(
                "the command itself exited with code {code}, and mlx-guard did not intervene; \
                 {code} is also one of mlx-guard's own exit codes."
            ),
            "read the command's own output; the report's outcome.kind is child_exited.",
            ANCHOR_COLLISION,
        )
    })
}

fn child_signaled(report: &ReportV1, signal: u8) -> Lines {
    let passed_on = report.signals.iter().any(|record| {
        record.reason == Some(SignalReason::ExternalSignal)
            && record.result == SignalResult::Delivered
            && record.signal == signal
    });
    let name = signal_name(signal);
    if passed_on {
        let happened = if signal == SIGKILL {
            "the command ended on SIGKILL, which mlx-guard sent after a second terminal signal \
             arrived."
                .to_owned()
        } else {
            format!("the command ended on {name}, which mlx-guard received and passed on to it.")
        };
        return Lines::new(happened, "nothing to do if you sent it.", ANCHOR_PASSED_ON);
    }
    let mut happened = format!("the command ended on {name}, and it did not come from mlx-guard");
    let to_do = if signal == SIGKILL {
        happened
            .push_str("; something else stopped it, and macOS can do this when memory runs short.");
        "check the command's own output and the macOS logs; if memory ran short, set \
         --max-footprint (with mlx-guard run) below that point so mlx-guard stops the command \
         first."
    } else {
        happened.push('.');
        "the command's own output or its macOS crash report says why."
    };
    Lines::new(happened, to_do, ANCHOR_ELSEWHERE)
}

fn launch_setting() -> Lines {
    Lines::new(
        "mlx-guard refused a launch setting before starting the command, so nothing ran; the \
         error line on stderr names the setting.",
        "--cwd must name an existing directory, and --env must not set MLX_GUARD_CHECKPOINT_FD; \
         fix the one named, and give the retry a new --report name, because this run kept its \
         report.",
        ANCHOR_LAUNCH_SETTING,
    )
}

fn not_executable() -> Lines {
    Lines::new(
        "the file named after -- exists but could not be run, so nothing was launched.",
        "make it executable (chmod +x) or put its interpreter first, as in -- python script.py; \
         give the retry a new --report name, because this run kept its report.",
        ANCHOR_NOT_EXECUTABLE,
    )
}

fn not_found() -> Lines {
    Lines::new(
        "no executable was found for the command named after --, so nothing was launched.",
        "check the spelling, give the full path, or activate the environment that provides it; \
         give the retry a new --report name, because this run kept its report.",
        ANCHOR_NOT_FOUND,
    )
}

fn supervisor_failure(report: &ReportV1) -> Lines {
    if report.samples.is_empty() && report.outcome.at_ms == 0 {
        return while_starting();
    }
    if report.configuration.mode == ReportMode::Enforce
        && let Some(lines) = not_delivered(report)
    {
        return lines;
    }
    let readings_lost = report
        .signals
        .iter()
        .any(|record| record.reason == Some(SignalReason::ObservationFailure))
        || (report.configuration.mode == ReportMode::Observe
            && report
                .transitions
                .iter()
                .any(|transition| transition.to == PolicyState::SupervisorError));
    if readings_lost {
        return readings_lost_row(report);
    }
    internal_failure(report)
}

fn while_starting() -> Lines {
    Lines::new(
        "mlx-guard could not finish starting the run, so it supervised nothing; the error line \
         on stderr names the step.",
        "run it again; if it repeats, open an issue with that error line and the redacted report.",
        ANCHOR_WHILE_STARTING,
    )
}

fn not_delivered(report: &ReportV1) -> Option<Lines> {
    let (signal, words) = report.signals.iter().find_map(|record| {
        if record.target != SignalTarget::OwnedProcessGroup {
            return None;
        }
        let words = match record.result {
            SignalResult::PermissionDenied => "permission denied",
            SignalResult::Failed => "failed",
            SignalResult::Delivered | SignalResult::ProcessMissing => return None,
        };
        Some((record.signal, words))
    })?;
    Some(Lines::new(
        format!(
            "mlx-guard could not deliver {} to the command's process group, which still had a \
             live member or could not be checked (result: {words}).",
            signal_name(signal)
        ),
        "check whether the command is still running and stop it yourself, then open an issue \
         with the redacted report.",
        ANCHOR_NOT_DELIVERED,
    ))
}

fn last_reading(report: &ReportV1) -> String {
    let words = match report
        .samples
        .last()
        .map(|sample| &sample.aggregate_footprint_bytes)
    {
        None | Some(Observed::Available { .. }) => return String::new(),
        Some(Observed::Error { code }) => match code {
            ObservationError::PermissionDenied => "permission denied",
            ObservationError::ProcessMissing => "a process disappeared",
            ObservationError::MalformedKernelData => "the kernel returned malformed data",
            ObservationError::ClockAnomaly => "the clock went backwards",
            ObservationError::Internal => "an internal error",
        },
        Some(Observed::Stale { .. }) => "the reading was too old",
        Some(Observed::Unavailable { .. } | Observed::Unknown) => "no reading",
    };
    format!(" (last reading: {words})")
}

fn readings_lost_row(report: &ReportV1) -> Lines {
    let lost = format!(
        "mlx-guard could not read the OS-accounted footprint for {} samples in a row{}",
        report.configuration.max_missing_samples,
        last_reading(report)
    );
    match report.configuration.mode {
        ReportMode::Enforce => Lines::new(
            format!(
                "{lost}, so it stopped the command rather than let it run unwatched: it {}.",
                action_clause(report)
            ),
            "run it again on a less loaded machine; if it repeats, open an issue with the \
             redacted report.",
            ANCHOR_READINGS_LOST,
        ),
        ReportMode::Observe => Lines::new(
            format!(
                "{lost}, so observation ended at {}. Observe mode does not stop the command, so \
                 it may still be running.",
                format_duration_ms(report.outcome.at_ms)
            ),
            "check whether the command is still running and stop it yourself if you need to, \
             then observe again.",
            ANCHOR_READINGS_LOST,
        ),
    }
}

fn internal_failure(report: &ReportV1) -> Lines {
    let term = report.signals.iter().find(|record| {
        is_group_signal(record, SIGTERM) && record.result == SignalResult::Delivered
    });
    let sent = term.map_or_else(String::new, |record| {
        format!(", and it {}", sent_to_group(SIGTERM, record.at_ms))
    });
    Lines::new(
        format!(
            "mlx-guard hit an internal failure while supervising{sent}; the error line on stderr \
             names it."
        ),
        "check whether the command is still running, then run it again; if it repeats, open an \
         issue with that error line and the redacted report.",
        ANCHOR_INTERNAL,
    )
}

fn policy_intervention(report: &ReportV1) -> Lines {
    emergency(report)
        .or_else(|| recorded_cause(report))
        .or_else(|| ignored_signal(report))
        .or_else(|| survivors(report))
        .unwrap_or_else(|| unrecorded(report))
}

fn emergency(report: &ReportV1) -> Option<Lines> {
    let transition = report.transitions.iter().find(|transition| {
        matches!(transition.from, PolicyState::Normal | PolicyState::Warning)
            && transition.to == PolicyState::Emergency
    })?;
    let value = transition.aggregate_footprint_bytes?;
    let threshold = report.configuration.emergency_footprint_bytes?;
    let limit = report.configuration.max_footprint_bytes?;
    let kill = first_group_signal(report, SIGKILL)?;
    let at = format_duration_ms(kill.at_ms);
    let action = match kill.result {
        SignalResult::Delivered => format!(
            "sent SIGKILL to the command's process group at once at {at}, with no checkpoint and \
             no grace period"
        ),
        SignalResult::ProcessMissing => {
            format!("found the command already ended when it sent SIGKILL at {at}")
        }
        SignalResult::PermissionDenied | SignalResult::Failed => return None,
    };
    let [value, threshold_text, limit_text] = compared_bytes([value, threshold, limit]);
    Some(Lines::new(
        format!(
            "one sample of the OS-accounted footprint, {value}, was at or above the emergency \
             threshold of {threshold_text}, which is {} more than your {limit_text} limit, so \
             mlx-guard {action}.",
            format_bytes(threshold.saturating_sub(limit)),
        ),
        "find what allocates that much between samples, or raise --max-footprint if that peak is \
         expected; a shorter --sample-interval sees growth sooner.",
        ANCHOR_EMERGENCY,
    ))
}

fn recorded_cause(report: &ReportV1) -> Option<Lines> {
    let cause = report.checkpoint.reason.or_else(|| {
        report.signals.iter().find_map(|record| {
            record.reason.filter(|reason| {
                matches!(
                    reason,
                    SignalReason::Footprint | SignalReason::WallTime | SignalReason::ParentExit
                )
            })
        })
    })?;
    match cause {
        SignalReason::Footprint => footprint(report),
        SignalReason::WallTime => wall_time(report),
        SignalReason::ParentExit => parent_exit(report),
        _ => None,
    }
}

fn footprint(report: &ReportV1) -> Option<Lines> {
    let limit = report.configuration.max_footprint_bytes?;
    let samples = report.configuration.required_breach_samples;
    let breach = report
        .transitions
        .iter()
        .find(|transition| {
            transition.from == PolicyState::Warning
                && matches!(
                    transition.to,
                    PolicyState::CheckpointRequested | PolicyState::Terminating
                )
        })
        .and_then(|transition| transition.aggregate_footprint_bytes);
    let measured = match breach {
        Some(value) => {
            let [value, limit] = compared_bytes([value, limit]);
            format!(
                "reached {value}, at or above the {limit} limit you set, on {samples} samples in a row"
            )
        }
        None => format!(
            "was at or above the {} limit you set on {samples} samples in a row",
            format_bytes(limit)
        ),
    };
    Some(Lines::new(
        format!(
            "the OS-accounted footprint {measured}, so mlx-guard {}.{}",
            action_clause(report),
            checkpoint_clause(report)
        ),
        format!(
            "observe the command to see its peak, then fix the growth or raise --max-footprint{}.",
            resume_clause(report)
        ),
        ANCHOR_FOOTPRINT,
    ))
}

fn wall_time(report: &ReportV1) -> Option<Lines> {
    let wall = report.configuration.wall_time_ms?;
    Some(Lines::new(
        format!(
            "the run reached the {} wall-time limit you set, so mlx-guard {}.{}",
            format_duration_ms(wall),
            action_clause(report),
            checkpoint_clause(report)
        ),
        format!(
            "raise --wall-time if the command needs longer, or find out why it ran longer than \
             expected{}.",
            resume_clause(report)
        ),
        ANCHOR_WALL,
    ))
}

fn parent_exit(report: &ReportV1) -> Option<Lines> {
    let parent = report.outcome.parent_exited_at_ms?;
    Some(Lines::new(
        format!(
            "the process that started mlx-guard exited at {}, and the default \
             --on-parent-exit=terminate stops the command with it, so mlx-guard {}.",
            format_duration_ms(parent),
            action_clause(report)
        ),
        "keep the launching process running until the run ends, or pass \
         --on-parent-exit=detach to let the command outlive it.",
        ANCHOR_PARENT,
    ))
}

/// Both the KILL and an earlier forwarded signal must have reached the group.
fn ignored_signal(report: &ReportV1) -> Option<Lines> {
    let kill_index = report.signals.iter().position(|record| {
        is_group_signal(record, SIGKILL) && record.reason == Some(SignalReason::ExternalSignal)
    })?;
    let kill = &report.signals[kill_index];
    if kill.result != SignalResult::Delivered {
        return None;
    }
    let forwarded = report.signals[..kill_index].iter().find(|record| {
        record.reason == Some(SignalReason::ExternalSignal)
            && record.result == SignalResult::Delivered
    })?;
    Some(Lines::new(
        format!(
            "the command was still running after the {} that mlx-guard passed on to it, so \
             mlx-guard sent SIGKILL to its process group at {}.",
            signal_name(forwarded.signal),
            format_duration_ms(kill.at_ms)
        ),
        "nothing, if you meant to stop it; otherwise make the command exit when it receives \
         SIGINT or SIGTERM.",
        ANCHOR_IGNORED_SIGNAL,
    ))
}

fn survivors(report: &ReportV1) -> Option<Lines> {
    let cleanup = report
        .signals
        .iter()
        .any(|record| record.reason == Some(SignalReason::RootExitCleanup));
    if !cleanup {
        return None;
    }
    let kill = first_group_signal(report, SIGKILL)?;
    let at = format_duration_ms(kill.at_ms);
    let action = match kill.result {
        SignalResult::Delivered => format!("sent SIGKILL to them at {at}"),
        SignalResult::ProcessMissing => {
            format!("found them already ended when it sent SIGKILL at {at}")
        }
        SignalResult::PermissionDenied | SignalResult::Failed => return None,
    };
    Some(Lines::new(
        format!(
            "the command exited, but processes it started were still running after SIGTERM, so \
             mlx-guard {action}; the command's own exit status is in the report under \
             outcome.child_status."
        ),
        "make the command wait for, or stop, the processes it starts before it exits.",
        ANCHOR_SURVIVORS,
    ))
}

/// The last fallback. When some record carries a reason that no rule matched, no limit caused the
/// stop; only a report with no reason anywhere (written before reasons existed) lacks the cause.
fn unrecorded(report: &ReportV1) -> Lines {
    let reasons_recorded = report.checkpoint.reason.is_some()
        || report.signals.iter().any(|record| record.reason.is_some());
    if reasons_recorded {
        return Lines::new(
            format!(
                "mlx-guard {}.{}",
                action_clause(report),
                checkpoint_clause(report)
            ),
            "read signals in the report to see what mlx-guard did; no limit was crossed.",
            ANCHOR_UNRECORDED,
        );
    }
    Lines::new(
        format!(
            "mlx-guard {}, and this report does not record which limit caused it.{}",
            action_clause(report),
            checkpoint_clause(report)
        ),
        "read signals and transitions in the report to see which limit was crossed.",
        ANCHOR_UNRECORDED,
    )
}

fn after_launch(detail: Option<&str>) -> Lines {
    let detail = detail.map_or_else(String::new, |words| format!(" ({words})"));
    Lines::new(
        format!(
            "the command was supervised, but mlx-guard could not write its complete \
             report{detail}; safety enforcement was not affected."
        ),
        "check free disk space and that nothing else changed the report directory during the \
         run; the hidden .<name>.journal beside the report path keeps what was saved. Use a new \
         --report name next time.",
        ANCHOR_AFTER_LAUNCH,
    )
}

// ---- Report-less rows (spec 5.2) ----

fn command_line(flag: Option<&'static str>) -> Lines {
    let shown = flag.map(|flag| {
        if flag == "<COMMAND>" {
            "the command after --"
        } else {
            flag
        }
    });
    let named = shown.map_or_else(String::new, |flag| format!(" ({flag})"));
    Lines::new(
        format!(
            "the command line was not accepted{named}, so nothing was launched and no report \
             was written."
        ),
        format!(
            "fix {} as the error above says, then run the command again.",
            shown.unwrap_or("the command line")
        ),
        ANCHOR_COMMAND_LINE,
    )
}

fn client_ready() -> Lines {
    Lines::new(
        "the internal --client-ready-fd descriptor could not be used, so nothing was launched \
         and no report was written.",
        "leave --client-ready-fd out when you run mlx-guard yourself; it is for the Python \
         client. If the Python client produced this, open an issue.",
        ANCHOR_COMMAND_LINE,
    )
}

fn configuration() -> Lines {
    Lines::new(
        "a setting was rejected before launch (the error above names it), so nothing was \
         launched and no report was written.",
        "fix that setting and run the command again.",
        ANCHOR_COMMAND_LINE,
    )
}

fn unsupported_platform() -> Lines {
    Lines::new(
        "mlx-guard needs macOS to read the OS-accounted footprint, so it launched nothing and \
         wrote no report.",
        "run it on a Mac with Apple Silicon.",
        ANCHOR_BEFORE_LAUNCH_70,
    )
}

fn footprint_unavailable() -> Lines {
    Lines::new(
        "mlx-guard could not read the OS-accounted footprint on this Mac, so it launched nothing \
         and wrote no report.",
        "check the compatibility page for your macOS version, then open an issue with your macOS \
         version and chip.",
        ANCHOR_BEFORE_LAUNCH_70,
    )
}

fn run_identity() -> Lines {
    Lines::new(
        "mlx-guard could not set up the run's identity, so it launched nothing and wrote no \
         report.",
        "make sure the command after -- names a file (an empty string or / does not); if it \
         does, open an issue.",
        ANCHOR_BEFORE_LAUNCH_70,
    )
}

fn supervisor_setup() -> Lines {
    Lines::new(
        "mlx-guard could not set up its own supervision, so it launched nothing and wrote no \
         report.",
        "run it again; if it repeats, open an issue with the error line above.",
        ANCHOR_BEFORE_LAUNCH_70,
    )
}

fn storage_detail(kind: StorageClass) -> Option<&'static str> {
    match kind {
        StorageClass::CreateFailed => Some("the journal could not be created"),
        StorageClass::WriteFailed => Some("a write failed"),
        StorageClass::SyncFailed => Some("a sync to disk failed"),
        StorageClass::ReadFailed => Some("reading the journal back failed"),
        StorageClass::InvalidRecord => Some("a journal record was invalid"),
        StorageClass::UnsafeTarget => Some("the report path is a link or another kind of file"),
        StorageClass::FinalizeFailed => Some("the final report could not be written"),
        StorageClass::InvalidPath
        | StorageClass::UnsafeDirectory
        | StorageClass::JournalExists
        | StorageClass::Unknown => None,
    }
}

fn storage(kind: StorageClass, phase: StoragePhase) -> Lines {
    if kind == StorageClass::JournalExists {
        return path_used();
    }
    match phase {
        StoragePhase::BeforeLaunch
            if matches!(
                kind,
                StorageClass::UnsafeDirectory | StorageClass::InvalidPath
            ) =>
        {
            report_directory()
        }
        StoragePhase::BeforeLaunch => before_launch(storage_detail(kind)),
        StoragePhase::LaunchFailed => launch_failed(),
        StoragePhase::StartUnfinished => start_unfinished(storage_detail(kind)),
        StoragePhase::AfterLaunch => after_launch(storage_detail(kind)),
    }
}

fn path_used() -> Lines {
    Lines::new(
        "an earlier run already used this report path, and its report and hidden journal are \
         kept, so nothing was launched.",
        "pass a new --report name, or move both the report and its hidden .<name>.journal away \
         before reusing the path.",
        ANCHOR_PATH_USED,
    )
}

fn report_directory() -> Lines {
    Lines::new(
        "the report directory is missing, not yours, or not owner-only (mode 0700), so nothing \
         was launched.",
        "create the directory with mkdir -m 700, or fix its owner and mode, then run again.",
        ANCHOR_DIRECTORY,
    )
}

fn before_launch(detail: Option<&str>) -> Lines {
    let detail = detail.map_or_else(String::new, |words| format!(" ({words})"));
    Lines::new(
        format!("mlx-guard could not prepare its journal{detail}, so nothing was launched."),
        "check free disk space, then pass a new --report name or move the hidden .<name>.journal \
         file away, and run again.",
        ANCHOR_BEFORE_LAUNCH_74,
    )
}

/// A run that did not finish starting and whose report could not be finished either.
///
/// It is neither a failed launch nor a supervised run: the command may or may not have started,
/// and the error lines printed before these name the step.
fn start_unfinished(detail: Option<&str>) -> Lines {
    let detail = detail.map_or_else(String::new, |words| format!(" ({words})"));
    Lines::new(
        format!(
            "mlx-guard could not finish starting the run, and could not write its report{detail}, \
             so it supervised nothing."
        ),
        "check free disk space and the report directory, then run again with a new --report \
         name; the error lines above name the step.",
        ANCHOR_AFTER_LAUNCH,
    )
}

fn launch_failed() -> Lines {
    Lines::new(
        "the command could not be launched, and mlx-guard could not finish the report about \
         that, so the cause is not recorded.",
        "check free disk space and the report directory, then run again with a new --report \
         name to see the launch error.",
        ANCHOR_AFTER_LAUNCH,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ANCHORS, EarlyCause, PlainOutcome, StorageClass, StoragePhase, doc_url, explain_early,
        explain_report, format_bytes, format_duration_ms, generic_early_cause, signal_name,
    };
    use crate::{
        ObservationError, Observed, PolicyState, ReportMode, ReportV1, SignalNumber,
        SupervisorOutcome, TerminalKind, TransitionRecord, UnavailableReason,
    };

    const URL_PREFIX: &str = "https://github.com/IonDen/mlx-guard/blob/v";

    fn base_report() -> ReportV1 {
        ReportV1::from_json(include_str!("../tests/fixtures/report-v1.json"))
            .expect("report-v1.json is a valid report")
    }

    fn assert_early(cause: EarlyCause, happened: &str, to_do: &str, anchor: &str) {
        let outcome = explain_early(cause);
        assert_eq!(outcome.what_happened, happened, "{cause:?}");
        assert_eq!(outcome.what_to_do, to_do, "{cause:?}");
        assert_eq!(outcome.anchor, anchor, "{cause:?}");
        assert!(ANCHORS.contains(&outcome.anchor), "{cause:?}");
    }

    fn storage(kind: StorageClass, phase: StoragePhase) -> EarlyCause {
        EarlyCause::Storage { kind, phase }
    }

    #[test]
    fn format_bytes_rounds_to_hundredths_of_the_largest_unit() {
        // Bug: truncation instead of rounding (6.04 GiB), or a float path that drifts from Python.
        assert_eq!(format_bytes(6_499_453_256), "6.05 GiB");
        assert_eq!(format_bytes(6_442_450_944), "6.00 GiB");
        assert_eq!(format_bytes(1_258_291), "1.20 MiB");
    }

    #[test]
    fn format_bytes_switches_units_at_1024() {
        // Bug: the KiB boundary off by one (`>` instead of `>=`), so 1024 prints `1024 B`.
        assert_eq!(format_bytes(100), "100 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.00 KiB");
    }

    #[test]
    fn format_bytes_steps_up_when_rounding_reaches_1024() {
        // Bug: the step-up rule missing, so one byte below 1 GiB prints `1024.00 MiB`.
        assert_eq!(format_bytes(1_073_741_823), "1.00 GiB");
        assert_eq!(format_bytes(1_048_575), "1.00 MiB");
    }

    #[test]
    fn equal_renders_of_different_values_fall_back_to_bytes() {
        // Bug: the equal-render rule missing, so a breach reads "6.00 GiB, at or above the
        // 6.00 GiB limit" for two different numbers.
        let mut report = base_report();
        report.checkpoint.status = crate::CheckpointStatus::NotNegotiated;
        report.checkpoint.at_ms = None;
        report.checkpoint.reason = Some(crate::SignalReason::Footprint);
        report.configuration.max_footprint_bytes = Some(6_442_450_944);
        report.transitions.push(TransitionRecord {
            at_ms: 13,
            from: PolicyState::Warning,
            to: PolicyState::Terminating,
            aggregate_footprint_bytes: Some(6_442_450_945),
        });
        let outcome = explain_report(&report).expect("a footprint intervention is explained");
        assert_eq!(
            outcome.what_happened,
            "the OS-accounted footprint reached 6442450945 bytes, at or above the 6442450944 \
             bytes limit you set, on 2 samples in a row, so mlx-guard sent SIGTERM to the \
             command's process group at 13 ms."
        );
    }

    #[test]
    fn equal_values_keep_their_units() {
        // Bug: the equal-render rule fires on two equal values, which render the same honestly.
        let mut report = base_report();
        report.checkpoint.status = crate::CheckpointStatus::NotNegotiated;
        report.checkpoint.at_ms = None;
        report.checkpoint.reason = Some(crate::SignalReason::Footprint);
        report.configuration.max_footprint_bytes = Some(6_442_450_944);
        report.transitions.push(TransitionRecord {
            at_ms: 13,
            from: PolicyState::Warning,
            to: PolicyState::CheckpointRequested,
            aggregate_footprint_bytes: Some(6_442_450_944),
        });
        let outcome = explain_report(&report).expect("a footprint intervention is explained");
        assert!(
            outcome.what_happened.starts_with(
                "the OS-accounted footprint reached 6.00 GiB, at or above the 6.00 GiB limit"
            ),
            "{}",
            outcome.what_happened
        );
    }

    #[test]
    fn kill_after_a_term_on_an_ended_group_keeps_its_suffix_after_a_checkpoint() {
        // Bug: the escalation suffix dropped after a checkpoint request whose TERM found the
        // group ended, so a delivered second-signal KILL goes unmentioned.
        use crate::{SignalReason, SignalRecord, SignalResult, SignalTarget};
        let mut report = base_report();
        report.signals = vec![
            SignalRecord {
                at_ms: 10,
                signal: 30,
                target: SignalTarget::CooperativeEndpoint,
                result: SignalResult::Delivered,
                reason: None,
            },
            SignalRecord {
                at_ms: 13,
                signal: 15,
                target: SignalTarget::OwnedProcessGroup,
                result: SignalResult::ProcessMissing,
                reason: None,
            },
            SignalRecord {
                at_ms: 1014,
                signal: 9,
                target: SignalTarget::OwnedProcessGroup,
                result: SignalResult::Delivered,
                reason: Some(SignalReason::ExternalSignal),
            },
        ];
        let outcome = explain_report(&report).expect("a policy intervention is explained");
        assert_eq!(
            outcome.what_happened,
            "mlx-guard asked the command to save a checkpoint at 10 ms; the command ended before \
             the SIGTERM that followed could reach it; a SIGKILL for a second terminal signal \
             reached its process group 1.0 s later. A checkpoint was requested but not \
             acknowledged."
        );
        assert_eq!(
            outcome.what_to_do,
            "read signals in the report to see what mlx-guard did; no limit was crossed."
        );
    }

    #[test]
    fn format_duration_ms_covers_every_unit() {
        // Bug: a branch boundary moved (`<=` for `<`), or minutes rounded from tenths.
        assert_eq!(format_duration_ms(13), "13 ms");
        assert_eq!(format_duration_ms(999), "999 ms");
        assert_eq!(format_duration_ms(1000), "1.0 s");
        assert_eq!(format_duration_ms(39_964), "40.0 s");
        assert_eq!(format_duration_ms(59_949), "59.9 s");
        assert_eq!(format_duration_ms(59_960), "1 min");
        assert_eq!(format_duration_ms(90_000), "1 min 30 s");
        assert_eq!(format_duration_ms(600_000), "10 min");
        assert_eq!(format_duration_ms(3_600_000), "1 h");
        assert_eq!(format_duration_ms(7_384_200), "2 h 3 min");
    }

    #[test]
    fn signal_name_uses_the_fixed_table() {
        // Bug: an entry missing or misspelled, or an unknown number printed as an empty string.
        let table = [
            (1, "SIGHUP"),
            (2, "SIGINT"),
            (3, "SIGQUIT"),
            (6, "SIGABRT"),
            (9, "SIGKILL"),
            (10, "SIGBUS"),
            (11, "SIGSEGV"),
            (13, "SIGPIPE"),
            (15, "SIGTERM"),
            (30, "SIGUSR1"),
        ];
        for (number, name) in table {
            assert_eq!(signal_name(number), name);
        }
        assert_eq!(signal_name(4), "signal 4");
        assert_eq!(signal_name(31), "signal 31");
    }

    #[cfg(unix)]
    #[test]
    fn signal_name_matches_the_portable_libc_numbers() {
        // Bug: a table number that differs from the platform's own signal constant.
        for (constant, name) in [
            (libc::SIGHUP, "SIGHUP"),
            (libc::SIGINT, "SIGINT"),
            (libc::SIGQUIT, "SIGQUIT"),
            (libc::SIGABRT, "SIGABRT"),
            (libc::SIGKILL, "SIGKILL"),
            (libc::SIGSEGV, "SIGSEGV"),
            (libc::SIGPIPE, "SIGPIPE"),
            (libc::SIGTERM, "SIGTERM"),
        ] {
            let number = u8::try_from(constant).expect("signal numbers fit in u8");
            assert_eq!(signal_name(number), name);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn signal_name_matches_the_darwin_only_numbers() {
        // Bug: SIGBUS or SIGUSR1 numbered as on Linux (7, 10) instead of Darwin (10, 30).
        for (constant, name) in [(libc::SIGBUS, "SIGBUS"), (libc::SIGUSR1, "SIGUSR1")] {
            let number = u8::try_from(constant).expect("signal numbers fit in u8");
            assert_eq!(signal_name(number), name);
        }
    }

    #[test]
    fn plain_outcome_forms_share_the_two_lines() {
        // Bug: the URL dropped from one form, or the stderr form missing its final newline.
        let outcome = PlainOutcome {
            what_happened: "a.".to_owned(),
            what_to_do: "b.".to_owned(),
            anchor: "70-before-launch",
            version: "1.2.3".to_owned(),
        };
        let url = "https://github.com/IonDen/mlx-guard/blob/v1.2.3/docs/TROUBLESHOOTING.md\
                   #70-before-launch";
        assert_eq!(
            outcome.text(),
            format!("what happened: a.\nwhat to do: b. More: {url}")
        );
        assert_eq!(
            outcome.stderr_lines(),
            format!("mlx-guard: what happened: a.\nmlx-guard: what to do: b. More: {url}\n")
        );
    }

    #[test]
    fn doc_url_pins_the_tag_and_the_anchor() {
        // Bug: the tag prefix `v` dropped, or the anchor not appended.
        assert_eq!(
            doc_url("1.2.3", "127-not-found"),
            "https://github.com/IonDen/mlx-guard/blob/v1.2.3/docs/TROUBLESHOOTING.md#127-not-found"
        );
    }

    #[test]
    fn report_backed_link_uses_the_report_package_version() {
        // Bug: the link built from VERSION, so a 0.1.0 fixture links the binary's tag and the two
        // languages disagree on the corpus.
        let mut report = base_report();
        report.package_version = "9.8.7".to_owned();
        let outcome = explain_report(&report).expect("a policy intervention is explained");
        assert_eq!(outcome.version, "9.8.7");
        assert!(
            outcome.text().ends_with(
                "More: https://github.com/IonDen/mlx-guard/blob/v9.8.7/docs/TROUBLESHOOTING.md\
                 #75-cause-not-recorded"
            ),
            "{}",
            outcome.text()
        );
    }

    #[test]
    fn early_cause_link_uses_the_binary_version() {
        // Bug: an early line built from an empty or placeholder version.
        let outcome = explain_early(EarlyCause::UnsupportedPlatform);
        let package = env!("CARGO_PKG_VERSION");
        assert!(!package.is_empty());
        assert_eq!(outcome.version, package);
        assert!(
            outcome.text().ends_with(&format!(
                "{URL_PREFIX}{package}/docs/TROUBLESHOOTING.md#70-before-launch"
            )),
            "{}",
            outcome.text()
        );
    }

    #[test]
    fn explain_report_partial_artifact_failure_uses_the_kind_fallback() {
        // Bug: the defensive 74 row panics, returns nothing, or invents a storage detail.
        let mut report = base_report();
        report.outcome.kind = TerminalKind::PartialArtifactFailure;
        let outcome = explain_report(&report).expect("exit 74 is explained");
        assert_eq!(
            outcome.what_happened,
            "the command was supervised, but mlx-guard could not write its complete report; \
             safety enforcement was not affected."
        );
        assert_eq!(
            outcome.what_to_do,
            "check free disk space and that nothing else changed the report directory during \
             the run; the hidden .<name>.journal beside the report path keeps what was saved. \
             Use a new --report name next time."
        );
        assert_eq!(outcome.anchor, "74-after-launch");
    }

    #[test]
    fn readings_lost_names_each_last_reading() {
        // Bug: an observation status mapped to the wrong words, or a clause printed with no sample.
        let mut report = base_report();
        report.outcome.kind = TerminalKind::SupervisorFailure;
        report.outcome.at_ms = 12_400;
        report.configuration.mode = ReportMode::Observe;
        report.signals.clear();
        report.transitions = vec![TransitionRecord {
            at_ms: 12_400,
            from: PolicyState::Observe,
            to: PolicyState::SupervisorError,
            aggregate_footprint_bytes: None,
        }];
        let cases = [
            (
                Observed::Error {
                    code: ObservationError::PermissionDenied,
                },
                " (last reading: permission denied)",
            ),
            (
                Observed::Error {
                    code: ObservationError::ProcessMissing,
                },
                " (last reading: a process disappeared)",
            ),
            (
                Observed::Error {
                    code: ObservationError::MalformedKernelData,
                },
                " (last reading: the kernel returned malformed data)",
            ),
            (
                Observed::Error {
                    code: ObservationError::ClockAnomaly,
                },
                " (last reading: the clock went backwards)",
            ),
            (
                Observed::Error {
                    code: ObservationError::Internal,
                },
                " (last reading: an internal error)",
            ),
            (
                Observed::Stale { last_seen_at_ms: 3 },
                " (last reading: the reading was too old)",
            ),
            (
                Observed::Unavailable {
                    reason: UnavailableReason::NotSupported,
                },
                " (last reading: no reading)",
            ),
            (Observed::Unknown, " (last reading: no reading)"),
        ];
        for (status, clause) in cases {
            let sample = report
                .samples
                .last_mut()
                .expect("the base report has a sample");
            sample.aggregate_footprint_bytes = status;
            let outcome = explain_report(&report).expect("lost readings are explained");
            assert_eq!(
                outcome.what_happened,
                format!(
                    "mlx-guard could not read the OS-accounted footprint for 3 samples in a \
                     row{clause}, so observation ended at 12.4 s. Observe mode does not stop the \
                     command, so it may still be running."
                )
            );
        }
        report.samples.clear();
        let outcome = explain_report(&report).expect("lost readings are explained");
        assert_eq!(
            outcome.what_happened,
            "mlx-guard could not read the OS-accounted footprint for 3 samples in a row, so \
             observation ended at 12.4 s. Observe mode does not stop the command, so it may \
             still be running."
        );
    }

    #[test]
    fn early_cause_command_line_renders_its_row() {
        // Bug: the whitelisted flag dropped, or a missing flag rendered as "()".
        assert_early(
            EarlyCause::CommandLine {
                flag: Some("--max-footprint"),
            },
            "the command line was not accepted (--max-footprint), so nothing was launched and no \
             report was written.",
            "fix --max-footprint as the error above says, then run the command again.",
            "64-the-command-line",
        );
        assert_early(
            EarlyCause::CommandLine { flag: None },
            "the command line was not accepted, so nothing was launched and no report was written.",
            "fix the command line as the error above says, then run the command again.",
            "64-the-command-line",
        );
    }

    #[test]
    fn command_token_renders_as_the_command_after_the_separator() {
        // Bug: clap's `<COMMAND>` placeholder printed verbatim into the plain line.
        assert_early(
            EarlyCause::CommandLine {
                flag: Some("<COMMAND>"),
            },
            "the command line was not accepted (the command after --), so nothing was launched \
             and no report was written.",
            "fix the command after -- as the error above says, then run the command again.",
            "64-the-command-line",
        );
    }

    #[test]
    fn early_cause_client_ready_descriptor_renders_its_row() {
        // Bug: the Python-only descriptor explained as a user mistake with no way out.
        assert_early(
            EarlyCause::ClientReadyDescriptor,
            "the internal --client-ready-fd descriptor could not be used, so nothing was launched \
             and no report was written.",
            "leave --client-ready-fd out when you run mlx-guard yourself; it is for the Python \
             client. If the Python client produced this, open an issue.",
            "64-the-command-line",
        );
    }

    #[test]
    fn early_cause_configuration_renders_its_row() {
        // Bug: the generic 64 claims a report was written.
        assert_early(
            EarlyCause::Configuration,
            "a setting was rejected before launch (the error above names it), so nothing was \
             launched and no report was written.",
            "fix that setting and run the command again.",
            "64-the-command-line",
        );
    }

    #[test]
    fn early_cause_unsupported_platform_renders_its_row() {
        // Bug: the platform refusal routed to the generic setup row.
        assert_early(
            EarlyCause::UnsupportedPlatform,
            "mlx-guard needs macOS to read the OS-accounted footprint, so it launched nothing and \
             wrote no report.",
            "run it on a Mac with Apple Silicon.",
            "70-before-launch",
        );
    }

    #[test]
    fn early_cause_footprint_unavailable_renders_its_row() {
        // Bug: an unreadable footprint on a Mac told to "run it on a Mac".
        assert_early(
            EarlyCause::FootprintUnavailable,
            "mlx-guard could not read the OS-accounted footprint on this Mac, so it launched \
             nothing and wrote no report.",
            "check the compatibility page for your macOS version, then open an issue with your \
             macOS version and chip.",
            "70-before-launch",
        );
    }

    #[test]
    fn early_cause_run_identity_renders_its_row() {
        // Bug: the run-identity failure explained as a generic setup failure.
        assert_early(
            EarlyCause::RunIdentity,
            "mlx-guard could not set up the run's identity, so it launched nothing and wrote no \
             report.",
            "make sure the command after -- names a file (an empty string or / does not); if it \
             does, open an issue.",
            "70-before-launch",
        );
    }

    #[test]
    fn early_cause_supervisor_setup_renders_its_row() {
        // Bug: the setup failure claims the command ran.
        assert_early(
            EarlyCause::SupervisorSetup,
            "mlx-guard could not set up its own supervision, so it launched nothing and wrote no \
             report.",
            "run it again; if it repeats, open an issue with the error line above.",
            "70-before-launch",
        );
    }

    #[test]
    fn early_cause_storage_renders_its_row() {
        // Bug: a pre-launch journal failure loses its detail words.
        assert_early(
            storage(StorageClass::WriteFailed, StoragePhase::BeforeLaunch),
            "mlx-guard could not prepare its journal (a write failed), so nothing was launched.",
            "check free disk space, then pass a new --report name or move the hidden \
             .<name>.journal file away, and run again.",
            "74-before-launch",
        );
    }

    #[test]
    fn unfinished_start_claims_neither_a_launch_failure_nor_supervision() {
        // Bug: a supervisor failure whose report was lost explained as a failed launch ("could
        // not be launched") or as a supervised run ("was supervised"), or under a new anchor.
        // Also: a claim that it stopped what it started, which not every route into this case
        // can back.
        assert_early(
            storage(StorageClass::InvalidRecord, StoragePhase::StartUnfinished),
            "mlx-guard could not finish starting the run, and could not write its report (a \
             journal record was invalid), so it supervised nothing.",
            "check free disk space and the report directory, then run again with a new --report \
             name; the error lines above name the step.",
            "74-after-launch",
        );
        let unknown = explain_early(storage(
            StorageClass::Unknown,
            StoragePhase::StartUnfinished,
        ));
        assert_eq!(
            unknown.what_happened,
            "mlx-guard could not finish starting the run, and could not write its report, so it \
             supervised nothing."
        );
    }

    #[test]
    fn storage_journal_exists_is_the_path_used_row_in_every_phase() {
        // Bug: the used-path check placed after the phase routing, so the retry trap returns.
        for phase in [
            StoragePhase::BeforeLaunch,
            StoragePhase::LaunchFailed,
            StoragePhase::StartUnfinished,
            StoragePhase::AfterLaunch,
        ] {
            assert_early(
                storage(StorageClass::JournalExists, phase),
                "an earlier run already used this report path, and its report and hidden journal \
                 are kept, so nothing was launched.",
                "pass a new --report name, or move both the report and its hidden \
                 .<name>.journal away before reusing the path.",
                "74-report-path-already-used",
            );
        }
    }

    #[test]
    fn storage_directory_kinds_are_the_directory_row_before_launch_only() {
        // Bug: the directory row matched without its phase, or only one of its two kinds.
        for kind in [StorageClass::UnsafeDirectory, StorageClass::InvalidPath] {
            assert_early(
                storage(kind, StoragePhase::BeforeLaunch),
                "the report directory is missing, not yours, or not owner-only (mode 0700), so \
                 nothing was launched.",
                "create the directory with mkdir -m 700, or fix its owner and mode, then run \
                 again.",
                "74-report-directory",
            );
        }
        let after = explain_early(storage(
            StorageClass::UnsafeDirectory,
            StoragePhase::AfterLaunch,
        ));
        assert_eq!(after.anchor, "74-after-launch");
    }

    #[test]
    fn storage_launch_failed_does_not_guess_the_cause() {
        // Bug: a launch-time storage failure explained with a storage detail it never had.
        assert_early(
            storage(StorageClass::ReadFailed, StoragePhase::LaunchFailed),
            "the command could not be launched, and mlx-guard could not finish the report about \
             that, so the cause is not recorded.",
            "check free disk space and the report directory, then run again with a new --report \
             name to see the launch error.",
            "74-after-launch",
        );
    }

    #[test]
    fn storage_after_launch_names_its_detail() {
        // Bug: the after-launch row drops the storage kind, or claims safety was affected.
        assert_early(
            storage(StorageClass::ReadFailed, StoragePhase::AfterLaunch),
            "the command was supervised, but mlx-guard could not write its complete report \
             (reading the journal back failed); safety enforcement was not affected.",
            "check free disk space and that nothing else changed the report directory during the \
             run; the hidden .<name>.journal beside the report path keeps what was saved. Use a \
             new --report name next time.",
            "74-after-launch",
        );
    }

    #[test]
    fn storage_detail_words_cover_every_kind() {
        // Bug: a kind given another kind's words, or an unknown kind rendered as "()".
        let words = [
            (
                StorageClass::CreateFailed,
                " (the journal could not be created)",
            ),
            (StorageClass::WriteFailed, " (a write failed)"),
            (StorageClass::SyncFailed, " (a sync to disk failed)"),
            (
                StorageClass::ReadFailed,
                " (reading the journal back failed)",
            ),
            (
                StorageClass::InvalidRecord,
                " (a journal record was invalid)",
            ),
            (
                StorageClass::UnsafeTarget,
                " (the report path is a link or another kind of file)",
            ),
            (
                StorageClass::FinalizeFailed,
                " (the final report could not be written)",
            ),
            (StorageClass::Unknown, ""),
        ];
        for (kind, detail) in words {
            let outcome = explain_early(storage(kind, StoragePhase::BeforeLaunch));
            assert_eq!(
                outcome.what_happened,
                format!(
                    "mlx-guard could not prepare its journal{detail}, so nothing was launched."
                ),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn generic_early_cause_maps_the_three_generic_codes() {
        // Bug: a child's own status or an intervention mapped to an early cause.
        assert_eq!(
            generic_early_cause(SupervisorOutcome::InvalidConfiguration),
            Some(EarlyCause::Configuration)
        );
        assert_eq!(
            generic_early_cause(SupervisorOutcome::SupervisorFailure),
            Some(EarlyCause::SupervisorSetup)
        );
        assert_eq!(
            generic_early_cause(SupervisorOutcome::PartialArtifactFailure),
            Some(storage(StorageClass::Unknown, StoragePhase::BeforeLaunch))
        );
        let signal = SignalNumber::new(9).expect("9 is a signal");
        for outcome in [
            SupervisorOutcome::ChildExited(64),
            SupervisorOutcome::ChildSignaled(signal),
            SupervisorOutcome::PolicyIntervention,
            SupervisorOutcome::LaunchNotFound,
            SupervisorOutcome::LaunchNotExecutable,
        ] {
            assert_eq!(generic_early_cause(outcome), None, "{outcome:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn storage_class_mirrors_every_storage_error_kind() {
        // Bug: two kinds swapped or collapsed in the platform-neutral mirror.
        use crate::StorageErrorKind;
        let pairs = [
            (StorageErrorKind::InvalidPath, StorageClass::InvalidPath),
            (
                StorageErrorKind::UnsafeDirectory,
                StorageClass::UnsafeDirectory,
            ),
            (StorageErrorKind::JournalExists, StorageClass::JournalExists),
            (StorageErrorKind::CreateFailed, StorageClass::CreateFailed),
            (StorageErrorKind::WriteFailed, StorageClass::WriteFailed),
            (StorageErrorKind::SyncFailed, StorageClass::SyncFailed),
            (StorageErrorKind::ReadFailed, StorageClass::ReadFailed),
            (StorageErrorKind::InvalidRecord, StorageClass::InvalidRecord),
            (StorageErrorKind::UnsafeTarget, StorageClass::UnsafeTarget),
            (
                StorageErrorKind::FinalizeFailed,
                StorageClass::FinalizeFailed,
            ),
        ];
        for (kind, class) in pairs {
            assert_eq!(StorageClass::from(kind), class, "{kind:?}");
        }
    }
}
