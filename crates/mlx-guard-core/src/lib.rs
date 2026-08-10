//! Platform-isolated core for the authoritative native supervisor.

mod platform;

pub use platform::{PlatformSupport, platform_support};

/// Package version supplied by the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
