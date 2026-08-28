use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::PolicyState;
#[cfg(unix)]
use crate::process_control::RootOutcome;

/// The only report schema major understood by this package.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// A typed observation that never confuses missing data with numeric zero.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Observed<T> {
    Available { value: T },
    Unknown,
    Unavailable { reason: UnavailableReason },
    Stale { last_seen_at_ms: u64 },
    Error { code: ObservationError },
}

/// Fixed reasons an observation may not exist.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    NotSupported,
    NotNegotiated,
    NotApplicable,
    NonUtf8,
}

/// Redacted observation failures safe to persist.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationError {
    PermissionDenied,
    ProcessMissing,
    MalformedKernelData,
    ClockAnomaly,
    Internal,
}

/// Privacy-safe identity fields derived from a command invocation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunIdentity {
    pub run_id: String,
    pub executable_basename: String,
    pub argument_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_hash: Option<String>,
}

impl RunIdentity {
    /// Project literal argv into the fields allowed by schema v1.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError`] for empty argv, unsafe run identifiers, invalid hashes, or an
    /// argument count that cannot be represented by the schema.
    pub fn from_argv(
        run_id: &str,
        argv: &[OsString],
        correlation_hash: Option<&str>,
    ) -> Result<Self, ReportError> {
        let executable = argv.first().ok_or(ReportError::InvalidIdentity)?;
        let basename = Path::new(executable)
            .file_name()
            .ok_or(ReportError::InvalidIdentity)?
            .to_str()
            .unwrap_or("<non-utf8>")
            .to_owned();
        let argument_count = u32::try_from(argv.len().saturating_sub(1))
            .map_err(|_| ReportError::InvalidIdentity)?;
        let identity = Self {
            run_id: run_id.to_owned(),
            executable_basename: basename,
            argument_count,
            correlation_hash: correlation_hash.map(ToOwned::to_owned),
        };
        identity.validate()?;
        Ok(identity)
    }

    fn validate(&self) -> Result<(), ReportError> {
        let valid_run_id = self.run_id.strip_prefix("run_").is_some_and(|suffix| {
            suffix.len() == 32
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if !valid_run_id
            || self.executable_basename.is_empty()
            || self.executable_basename.len() > 255
            || matches!(self.executable_basename.as_str(), "." | "..")
            || self.executable_basename.contains(['/', '\\'])
            || self
                .correlation_hash
                .as_deref()
                .is_some_and(|hash| !valid_correlation_hash(hash))
        {
            return Err(ReportError::InvalidIdentity);
        }
        Ok(())
    }
}

fn valid_correlation_hash(value: &str) -> bool {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_package_version(value: &str) -> bool {
    let core_end = value.find(['-', '+']).unwrap_or(value.len());
    let core = &value[..core_end];
    let mut parts = core.split('.');
    let numbers_valid = (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none();
    numbers_valid
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

/// Capabilities captured before command launch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Capabilities {
    pub darwin_footprint: Observed<bool>,
    pub owned_process_group: Observed<bool>,
    pub checkpoint_channel: Observed<bool>,
}

/// Public operating mode recorded in a report.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportMode {
    Observe,
    Enforce,
}

/// Normalized, non-secret policy configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReportConfiguration {
    pub mode: ReportMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_footprint_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_footprint_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_footprint_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emergency_footprint_bytes: Option<u64>,
    pub required_breach_samples: u32,
    pub max_missing_samples: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_time_ms: Option<u64>,
    pub sample_interval_ms: u64,
    pub max_sample_age_ms: u64,
    pub max_sample_window_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint_timeout_ms: Option<u64>,
    pub term_grace_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_parent_exit: Option<OnParentExit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_watch: Option<ParentWatch>,
}

/// The requested behavior when the supervisor's own parent process exits.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnParentExit {
    Terminate,
    Detach,
}

/// Whether, and how, the supervisor is watching for its parent's exit.
///
/// `active` = watching and enforcing; `detach` = watching for evidence only;
/// `parent_is_launchd` / `hangup_ignored` = never checked; `parent_unobservable` = the
/// launch-time parent inspection failed, never checked.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParentWatch {
    Active,
    ParentIsLaunchd,
    HangupIgnored,
    Detach,
    ParentUnobservable,
}

/// Advisory values stored beside, but never used as, v0.1 policy input.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisoryScope {
    System,
    OwnedProcessGroup,
}

/// Stable source names for advisory observations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisorySource {
    DispatchMemoryPressure,
    SysctlVmSwapusage,
    HostStatistics64,
    DerivedFootprintSamples,
}

/// Freshness of an advisory value at the time it was projected into a sample window.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisoryFreshness {
    Fresh,
    InitialUnknown,
    Stale,
    Unavailable,
}

/// System memory-pressure level reported by an operating-system event source.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressureLevel {
    Normal,
    Warning,
    Critical,
}

/// Scope, source, timestamp, and freshness attached to one additive advisory field.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdvisoryMetricMetadata {
    pub scope: AdvisoryScope,
    pub source: AdvisorySource,
    pub captured_at_ms: u64,
    pub freshness: AdvisoryFreshness,
}

/// Per-field metadata added without changing the frozen schema-v1 value shapes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdvisoryMetadata {
    pub pressure_events: AdvisoryMetricMetadata,
    pub pressure_level: AdvisoryMetricMetadata,
    pub swap_bytes: AdvisoryMetricMetadata,
    pub compressor_bytes: AdvisoryMetricMetadata,
    pub wired_bytes: AdvisoryMetricMetadata,
    pub growth_bytes_per_second: AdvisoryMetricMetadata,
}

/// Advisory values stored beside, but never used as, v0.1 policy input.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdvisoryMetrics {
    pub pressure_events: Observed<u64>,
    pub swap_bytes: Observed<u64>,
    pub compressor_bytes: Observed<u64>,
    pub wired_bytes: Observed<u64>,
    pub growth_bytes_per_second: Observed<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_level: Option<Observed<MemoryPressureLevel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<AdvisoryMetadata>,
}

/// One bounded aggregate sampling window.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SampleWindow {
    pub captured_at_ms: u64,
    pub processed_at_ms: u64,
    pub window_ms: u64,
    pub aggregate_footprint_bytes: Observed<u64>,
    pub advisory: AdvisoryMetrics,
}

/// One state transition requested by the pure policy machine.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransitionRecord {
    pub at_ms: u64,
    pub from: PolicyState,
    pub to: PolicyState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate_footprint_bytes: Option<u64>,
}

/// The only signal targets represented by schema v1.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalTarget {
    OwnedProcessGroup,
    CooperativeEndpoint,
}

/// Redacted result of a signal-delivery attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalResult {
    Delivered,
    ProcessMissing,
    PermissionDenied,
    Failed,
}

/// Why the supervisor sent one signal record.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalReason {
    Footprint,
    WallTime,
    ExternalSignal,
    RootExitCleanup,
    ParentExit,
    ObservationFailure,
    SupervisorFault,
}

/// One signal attempt and its observed system-call result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SignalRecord {
    pub at_ms: u64,
    pub signal: u8,
    pub target: SignalTarget,
    pub result: SignalResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<SignalReason>,
}

/// Worker checkpoint evidence, not a durability assertion.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    NotNegotiated,
    RequestedUnverified,
    AcknowledgedUnverifiedDurability,
    TimedOut,
    Cancelled,
}

/// Final checkpoint protocol state and the time it was observed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointRecord {
    pub status: CheckpointStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
    /// The nonzero request id the supervisor sent and, on acknowledgement, the worker echoed —
    /// the correlation key a worker tags its own saved state with. Absent from reports written
    /// before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<u64>,
    /// The intervention cause behind the checkpoint attempt, present whenever a checkpoint
    /// actuation was executed toward the request, delivered or not. Absent from reports written
    /// before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<SignalReason>,
    /// Worker-reported, path-free artifact facts echoed from the acknowledgement frame — the
    /// worker's report, not independent proof. Absent from reports written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<CheckpointArtifactRecord>,
}

/// Path-free classification of a worker-reported checkpoint artifact, mirroring the wire's
/// `CheckpointArtifactKind` (checkpoint.rs) but kept as a distinct report-side type: the wire
/// enum of the same name is re-exported at the crate root, so a report enum sharing that name
/// would collide there on unix builds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Directory,
    Opaque,
}

/// Worker-reported, path-free artifact facts echoed from a checkpoint acknowledgement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointArtifactRecord {
    pub kind: ArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// Best-effort evidence that a descendant escaped the owned group.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EscapeEvidence {
    pub detected: Observed<bool>,
    /// Escapes observation counted: at least one increment per distinct escaped identity, and
    /// independent of the bounded in-memory evidence list, so truncation is counted rather than
    /// silent. Above that cap the counted mark lives only on the tracked set, so an identity a
    /// sample misses and later re-observes can add a further increment; this is bounded evidence
    /// of distinct escapes, not an exact census. Present only when nonzero. Only the count is
    /// persisted, never the escapees' pids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escaped_count: Option<u64>,
}

/// Redacted persistence failures.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactErrorCode {
    CreateFailed,
    WriteFailed,
    SyncFailed,
    RenameFailed,
    PermissionFailed,
}

/// One persistence failure without its sensitive path or operating-system message.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactErrorRecord {
    pub at_ms: u64,
    pub code: ArtifactErrorCode,
}

/// Typed terminal result. Child status fields stay separate from supervisor outcomes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalKind {
    ChildExited { code: u8 },
    ChildSignaled { signal: u8 },
    LaunchNotFound,
    LaunchNotExecutable,
    InvalidConfiguration,
    PolicyIntervention,
    SupervisorFailure,
    PartialArtifactFailure,
}

/// The root command's own exit status, recorded independently of the supervisor outcome.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ChildStatus {
    Exited { code: u8 },
    Signaled { signal: u8 },
}

#[cfg(unix)]
impl From<RootOutcome> for ChildStatus {
    fn from(outcome: RootOutcome) -> Self {
        match outcome {
            RootOutcome::Exited(code) => Self::Exited { code },
            RootOutcome::Signaled(signal) => Self::Signaled {
                signal: signal.get(),
            },
        }
    }
}

/// Final observed outcome and footprint availability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalOutcome {
    pub at_ms: u64,
    #[serde(flatten)]
    pub kind: TerminalKind,
    pub final_footprint_bytes: Observed<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_status: Option<ChildStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_group_survivors: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_exited_at_ms: Option<u64>,
}

/// Upload behavior frozen for v0.1.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadPolicy {
    Disabled,
}

/// Retention behavior frozen for v0.1.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPolicy {
    UserManaged,
}

/// Sensitive capture surface frozen for v0.1.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePolicy {
    RedactedMetadataOnly,
    RedactedMetadataWithCorrelationHash,
}

/// Privacy assertions that every schema-v1 writer must uphold before persistence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrivacyDefaults {
    pub redacted_before_persistence: bool,
    pub file_mode: String,
    pub upload: UploadPolicy,
    pub retention: RetentionPolicy,
    pub capture: CapturePolicy,
}

impl Default for PrivacyDefaults {
    fn default() -> Self {
        Self {
            redacted_before_persistence: true,
            file_mode: "0600".to_owned(),
            upload: UploadPolicy::Disabled,
            retention: RetentionPolicy::UserManaged,
            capture: CapturePolicy::RedactedMetadataOnly,
        }
    }
}

/// Complete schema-v1 report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReportV1 {
    pub schema_version: u32,
    pub package_version: String,
    pub run: RunIdentity,
    pub capabilities: Capabilities,
    pub configuration: ReportConfiguration,
    pub samples: Vec<SampleWindow>,
    pub transitions: Vec<TransitionRecord>,
    pub signals: Vec<SignalRecord>,
    pub checkpoint: CheckpointRecord,
    pub escape: EscapeEvidence,
    pub artifact_errors: Vec<ArtifactErrorRecord>,
    pub outcome: TerminalOutcome,
    pub privacy: PrivacyDefaults,
}

impl ReportV1 {
    /// Validate schema, privacy, ordering, and cross-field invariants.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ReportError`] when the report cannot be safely written as schema v1.
    pub fn validate(&self) -> Result<(), ReportError> {
        if self.schema_version != REPORT_SCHEMA_VERSION {
            return Err(ReportError::UnsupportedSchema);
        }
        if !valid_package_version(&self.package_version) {
            return Err(ReportError::InvalidPackageVersion);
        }
        self.run.validate()?;
        self.validate_configuration()?;
        self.validate_privacy()?;
        self.validate_events()?;
        Ok(())
    }

    /// Serialize a validated report with stable field order and a final newline.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError`] if validation or JSON serialization fails.
    pub fn to_json_pretty(&self) -> Result<String, ReportError> {
        self.validate()?;
        let mut encoded = serde_json::to_string_pretty(self).map_err(ReportError::Json)?;
        encoded.push('\n');
        Ok(encoded)
    }

    /// Parse schema v1 while ignoring unknown fields for forward-compatible readers.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError`] for malformed JSON, unsupported schema majors, or invalid values.
    pub fn from_json(value: &str) -> Result<Self, ReportError> {
        let report: Self = serde_json::from_str(value).map_err(ReportError::Json)?;
        report.validate()?;
        Ok(report)
    }

    fn validate_configuration(&self) -> Result<(), ReportError> {
        let basic_values_valid = self.configuration.sample_interval_ms > 0
            && self.configuration.required_breach_samples > 0
            && self.configuration.max_missing_samples > 0
            && self.configuration.max_sample_age_ms > 0
            && self.configuration.max_sample_window_ms > 0
            && self.configuration.term_grace_ms > 0
            && self
                .configuration
                .checkpoint_timeout_ms
                .is_none_or(|value| value > 0)
            && self
                .configuration
                .wall_time_ms
                .is_none_or(|value| value > 0);
        let mode_valid = match self.configuration.mode {
            ReportMode::Observe => {
                self.configuration.max_footprint_bytes.is_none()
                    && self.configuration.warning_footprint_bytes.is_none()
                    && self.configuration.recovery_footprint_bytes.is_none()
                    && self.configuration.emergency_footprint_bytes.is_none()
                    && self.configuration.wall_time_ms.is_none()
                    && self.configuration.checkpoint_timeout_ms.is_none()
            }
            ReportMode::Enforce => match (
                self.configuration.recovery_footprint_bytes,
                self.configuration.warning_footprint_bytes,
                self.configuration.max_footprint_bytes,
                self.configuration.emergency_footprint_bytes,
            ) {
                (Some(recovery), Some(warning), Some(limit), Some(emergency)) => {
                    recovery < warning && warning < limit && limit < emergency
                }
                _ => false,
            },
        };
        let parent_watch_agrees_with_option =
            !matches!(self.configuration.parent_watch, Some(ParentWatch::Detach))
                || self.configuration.on_parent_exit == Some(OnParentExit::Detach);
        let detach_option_agrees_with_watch = self.configuration.on_parent_exit
            != Some(OnParentExit::Detach)
            || matches!(
                self.configuration.parent_watch,
                Some(ParentWatch::Detach | ParentWatch::ParentUnobservable)
            );
        let parent_exit_evidence_has_a_watch = self.outcome.parent_exited_at_ms.is_none()
            || matches!(
                self.configuration.parent_watch,
                Some(ParentWatch::Active | ParentWatch::Detach)
            );
        if !basic_values_valid
            || !mode_valid
            || !parent_watch_agrees_with_option
            || !detach_option_agrees_with_watch
            || !parent_exit_evidence_has_a_watch
        {
            return Err(ReportError::InvalidConfiguration);
        }
        Ok(())
    }

    fn validate_privacy(&self) -> Result<(), ReportError> {
        if !self.privacy.redacted_before_persistence
            || self.privacy.file_mode != "0600"
            || self.privacy.upload != UploadPolicy::Disabled
            || self.privacy.retention != RetentionPolicy::UserManaged
            || self.privacy.capture
                != if self.run.correlation_hash.is_some() {
                    CapturePolicy::RedactedMetadataWithCorrelationHash
                } else {
                    CapturePolicy::RedactedMetadataOnly
                }
        {
            return Err(ReportError::InvalidPrivacy);
        }
        Ok(())
    }

    fn validate_events(&self) -> Result<(), ReportError> {
        let samples_ordered = self.samples.windows(2).all(|pair| {
            pair[0].processed_at_ms <= pair[1].processed_at_ms
                && pair[0].captured_at_ms <= pair[0].processed_at_ms
                && pair[0].window_ms > 0
        }) && self.samples.last().is_none_or(|sample| {
            sample.captured_at_ms <= sample.processed_at_ms && sample.window_ms > 0
        });
        let advisory_valid = self.samples.iter().all(valid_advisory_metrics);
        let transitions_ordered = ordered_by(&self.transitions, |item| item.at_ms)
            && self.transitions.iter().all(|item| item.from != item.to);
        let signals_ordered = ordered_by(&self.signals, |item| item.at_ms)
            && self
                .signals
                .iter()
                .all(|item| (1..=127).contains(&item.signal));
        let artifacts_ordered = ordered_by(&self.artifact_errors, |item| item.at_ms);
        let checkpoint_valid = match self.checkpoint.status {
            CheckpointStatus::NotNegotiated => self.checkpoint.at_ms.is_none(),
            CheckpointStatus::RequestedUnverified
            | CheckpointStatus::AcknowledgedUnverifiedDurability
            | CheckpointStatus::TimedOut
            | CheckpointStatus::Cancelled => self.checkpoint.at_ms.is_some(),
        };
        // Present-⇒-constrained only: schema v1 validates on read, so no rule here may require a
        // field's absence for any status (every 0.1.0 report, including the committed golden,
        // must keep parsing).
        let checkpoint_request_id_valid = self.checkpoint.request_id.is_none_or(|id| id != 0);
        let checkpoint_request_id_status_valid = self.checkpoint.request_id.is_none_or(|_| {
            matches!(
                self.checkpoint.status,
                CheckpointStatus::RequestedUnverified
                    | CheckpointStatus::AcknowledgedUnverifiedDurability
                    | CheckpointStatus::TimedOut
            )
        });
        let checkpoint_artifact_status_valid = self.checkpoint.artifact.is_none()
            || self.checkpoint.status == CheckpointStatus::AcknowledgedUnverifiedDurability;
        let checkpoint_reason_valid = self.checkpoint.reason.is_none_or(|reason| {
            matches!(reason, SignalReason::Footprint | SignalReason::WallTime)
        });
        let terminal_signal_valid = match self.outcome.kind {
            TerminalKind::ChildSignaled { signal } => (1..=127).contains(&signal),
            _ => true,
        };
        let child_status_signal_valid = !matches!(
            self.outcome.child_status,
            Some(ChildStatus::Signaled { signal }) if !(1..=127).contains(&signal)
        );
        let child_status_agrees = match (&self.outcome.kind, self.outcome.child_status) {
            (TerminalKind::ChildExited { code }, Some(status)) => {
                status == ChildStatus::Exited { code: *code }
            }
            (TerminalKind::ChildSignaled { signal }, Some(status)) => {
                status == ChildStatus::Signaled { signal: *signal }
            }
            _ => true,
        };
        let parent_exit_evidence_before_the_outcome = self
            .outcome
            .parent_exited_at_ms
            .is_none_or(|at_ms| at_ms <= self.outcome.at_ms);
        let escaped_count_agrees_with_detection = self.escape.escaped_count.is_none_or(|count| {
            count > 0 && matches!(self.escape.detected, Observed::Available { value: true })
        });
        let last_event = self
            .samples
            .iter()
            .map(|item| item.processed_at_ms)
            .chain(self.transitions.iter().map(|item| item.at_ms))
            .chain(self.signals.iter().map(|item| item.at_ms))
            .chain(self.artifact_errors.iter().map(|item| item.at_ms))
            .chain(self.checkpoint.at_ms)
            .max()
            .unwrap_or(0);

        if !advisory_valid {
            return Err(ReportError::InvalidAdvisoryMetrics);
        }
        if !samples_ordered
            || !transitions_ordered
            || !signals_ordered
            || !artifacts_ordered
            || !checkpoint_valid
            || !checkpoint_request_id_valid
            || !checkpoint_request_id_status_valid
            || !checkpoint_artifact_status_valid
            || !checkpoint_reason_valid
            || !terminal_signal_valid
            || !child_status_signal_valid
            || !child_status_agrees
            || !parent_exit_evidence_before_the_outcome
            || !escaped_count_agrees_with_detection
            || self.outcome.at_ms < last_event
        {
            return Err(ReportError::InvalidEventOrder);
        }
        Ok(())
    }
}

fn valid_advisory_metrics(sample: &SampleWindow) -> bool {
    let (Some(pressure_level), Some(metadata)) =
        (&sample.advisory.pressure_level, &sample.advisory.metadata)
    else {
        return sample.advisory.pressure_level.is_none() && sample.advisory.metadata.is_none();
    };
    let expected = [
        (
            &metadata.pressure_events,
            AdvisoryScope::System,
            AdvisorySource::DispatchMemoryPressure,
            freshness_for(&sample.advisory.pressure_events),
        ),
        (
            &metadata.pressure_level,
            AdvisoryScope::System,
            AdvisorySource::DispatchMemoryPressure,
            freshness_for(pressure_level),
        ),
        (
            &metadata.swap_bytes,
            AdvisoryScope::System,
            AdvisorySource::SysctlVmSwapusage,
            freshness_for(&sample.advisory.swap_bytes),
        ),
        (
            &metadata.compressor_bytes,
            AdvisoryScope::System,
            AdvisorySource::HostStatistics64,
            freshness_for(&sample.advisory.compressor_bytes),
        ),
        (
            &metadata.wired_bytes,
            AdvisoryScope::System,
            AdvisorySource::HostStatistics64,
            freshness_for(&sample.advisory.wired_bytes),
        ),
        (
            &metadata.growth_bytes_per_second,
            AdvisoryScope::OwnedProcessGroup,
            AdvisorySource::DerivedFootprintSamples,
            freshness_for(&sample.advisory.growth_bytes_per_second),
        ),
    ];
    expected.iter().all(|(metadata, scope, source, freshness)| {
        metadata.scope == *scope
            && metadata.source == *source
            && metadata.freshness == *freshness
            && metadata.captured_at_ms <= sample.processed_at_ms
    })
}

fn freshness_for<T>(observation: &Observed<T>) -> AdvisoryFreshness {
    match observation {
        Observed::Available { .. } => AdvisoryFreshness::Fresh,
        Observed::Unknown => AdvisoryFreshness::InitialUnknown,
        Observed::Stale { .. } => AdvisoryFreshness::Stale,
        Observed::Unavailable { .. } | Observed::Error { .. } => AdvisoryFreshness::Unavailable,
    }
}

fn ordered_by<T>(items: &[T], timestamp: impl Fn(&T) -> u64) -> bool {
    items
        .windows(2)
        .all(|pair| timestamp(&pair[0]) <= timestamp(&pair[1]))
}

/// Validation or serialization failure without sensitive source text.
#[derive(Debug)]
pub enum ReportError {
    InvalidIdentity,
    UnsupportedSchema,
    InvalidPackageVersion,
    InvalidConfiguration,
    InvalidPrivacy,
    InvalidEventOrder,
    InvalidAdvisoryMetrics,
    Json(serde_json::Error),
}

impl fmt::Display for ReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidIdentity => "invalid redacted run identity",
            Self::UnsupportedSchema => "unsupported report schema version",
            Self::InvalidPackageVersion => "invalid package version",
            Self::InvalidConfiguration => "invalid report configuration",
            Self::InvalidPrivacy => "report privacy defaults were weakened",
            Self::InvalidEventOrder => "invalid report event ordering",
            Self::InvalidAdvisoryMetrics => "invalid advisory metric metadata",
            Self::Json(_) => "report JSON is malformed",
        };
        formatter.write_str(message)
    }
}

impl Error for ReportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{ChildStatus, RootOutcome};
    use crate::SignalNumber;

    #[test]
    fn from_root_outcome_maps_an_exited_root_to_the_exited_arm() {
        assert_eq!(
            ChildStatus::from(RootOutcome::Exited(3)),
            ChildStatus::Exited { code: 3 }
        );
    }

    #[test]
    fn from_root_outcome_maps_a_signaled_root_to_the_signaled_arm() {
        let signal = SignalNumber::new(9).unwrap();
        assert_eq!(
            ChildStatus::from(RootOutcome::Signaled(signal)),
            ChildStatus::Signaled { signal: 9 }
        );
    }
}
