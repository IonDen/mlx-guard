//! Platform-isolated core for the authoritative native supervisor.

#[cfg(unix)]
mod advisory;
#[cfg(unix)]
mod checkpoint;
#[cfg(unix)]
mod identity;
mod intervention;
mod outcome;
#[cfg(unix)]
mod persistence;
mod platform;
mod policy;
#[cfg(unix)]
mod process_control;
mod report;
#[cfg(unix)]
mod sampling;

#[cfg(unix)]
pub use checkpoint::{
    CHECKPOINT_FD_ENV, CHECKPOINT_PROTOCOL_VERSION, CheckpointAcknowledgement,
    CheckpointArtifactKind, CheckpointArtifactMetadata, CheckpointChannel, CheckpointChannelError,
    CheckpointChannelRequestError, CheckpointHello, CheckpointNonce, CheckpointPoll,
    CheckpointProtocol, CheckpointProtocolError, CheckpointProtocolState, CheckpointRejection,
    CheckpointRequest, CheckpointSignalConfig, CheckpointSignalConfigError,
    CheckpointWorkerEndpoint, CheckpointWorkerStatus, MAX_CHECKPOINT_FRAME_BYTES,
};
#[cfg(unix)]
pub use identity::{
    AggregateFootprint, CleanupReport, ContainmentEvent, IdentityTracker, IdentityUnavailable,
    NativeProcessInventory, ObservationFailure, ObservationFailureKind, ProcessIdentity,
    ProcessObservation, ProcessSnapshot, SnapshotError, TrackingFrame, wait_for_owned_group_empty,
};
pub use intervention::{
    Actuation, ActuationOutcome, InterventionActuator, InterventionEngine, InterventionEvidence,
    InterventionProgress, InterventionRecord, MAX_INTERVENTION_RECORDS,
};
#[cfg(unix)]
pub use intervention::{CheckpointBinding, CheckpointObservation, ProcessInterventionActuator};
pub use outcome::{SignalNumber, SupervisorOutcome};
#[cfg(unix)]
pub use persistence::{
    FinalizedArtifacts, JOURNAL_MAGIC, JOURNAL_RECORD_VERSION, JournalAppender, JournalDurability,
    JournalEntry, JournalHeader, JournalRecord, JournalRecovery, JournalRecoveryStatus,
    MAX_JOURNAL_RECORD_BYTES, PersistenceAttempt, ResilientJournal, SecureJournal, StorageError,
    StorageErrorKind,
};
pub use platform::{PlatformSupport, platform_support};
pub use policy::{
    Action, ActuationFailure, ActuationKind, CheckpointDisposition, Event, POLICY_CONTRACT_VERSION,
    PolicyConfig, PolicyConfigError, PolicyMachine, PolicyState, SampleEvent,
};
#[cfg(unix)]
pub use process_control::{
    CheckpointEndpoint, ClientReady, ControlError, ControlErrorKind, LaunchError, LaunchErrorKind,
    LaunchOptions, OwnedProcess, ProcessControlHandle, RootOutcome, StdioMode,
    TerminalSignalMonitor, checkpoint_signal_usr1, validate_noninteractive_terminal,
};
pub use report::{
    AdvisoryFreshness, AdvisoryMetadata, AdvisoryMetricMetadata, AdvisoryMetrics, AdvisoryScope,
    AdvisorySource, ArtifactErrorCode, ArtifactErrorRecord, Capabilities, CapturePolicy,
    CheckpointRecord, CheckpointStatus, EscapeEvidence, MemoryPressureLevel, ObservationError,
    Observed, PrivacyDefaults, REPORT_SCHEMA_VERSION, ReportConfiguration, ReportError, ReportMode,
    ReportV1, RetentionPolicy, RunIdentity, SampleWindow, SignalRecord, SignalResult, SignalTarget,
    TerminalKind, TerminalOutcome, TransitionRecord, UnavailableReason, UploadPolicy,
};
#[cfg(unix)]
pub use sampling::{
    FootprintSample, FootprintSampler, MAX_SAMPLE_HISTORY_CAPACITY, MAX_SAMPLE_INTERVAL,
    MIN_SAMPLE_INTERVAL, ProcessFootprintSample, SampleOutcome, SamplingClockError, SamplingConfig,
    SamplingConfigError,
};

/// Package version supplied by the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
#[cfg(unix)]
pub use advisory::{
    AdvisorySnapshot, CALIBRATION_SCHEMA_VERSION, CalibrationArtifact, CalibrationError,
    CalibrationGuidance, NativeAdvisoryObserver, ObserveCalibration, PrelaunchSummary,
    PrelaunchWarning,
};
