//! Platform-isolated core for the authoritative native supervisor.

mod outcome;
mod platform;
mod policy;
mod report;

pub use outcome::{SignalNumber, SupervisorOutcome};
pub use platform::{PlatformSupport, platform_support};
pub use policy::{
    Action, CheckpointDisposition, Event, PolicyConfig, PolicyConfigError, PolicyMachine,
    PolicyState, SampleEvent,
};
pub use report::{
    AdvisoryMetrics, ArtifactErrorCode, ArtifactErrorRecord, Capabilities, CapturePolicy,
    CheckpointRecord, CheckpointStatus, EscapeEvidence, ObservationError, Observed,
    PrivacyDefaults, REPORT_SCHEMA_VERSION, ReportConfiguration, ReportError, ReportMode, ReportV1,
    RetentionPolicy, RunIdentity, SampleWindow, SignalRecord, SignalResult, SignalTarget,
    TerminalKind, TerminalOutcome, TransitionRecord, UnavailableReason, UploadPolicy,
};

/// Package version supplied by the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
