//! Platform-isolated core for the authoritative native supervisor.

mod outcome;
mod platform;
mod policy;

pub use outcome::{SignalNumber, SupervisorOutcome};
pub use platform::{PlatformSupport, platform_support};
pub use policy::{
    Action, CheckpointDisposition, Event, PolicyConfig, PolicyConfigError, PolicyMachine,
    PolicyState, SampleEvent,
};

/// Package version supplied by the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
