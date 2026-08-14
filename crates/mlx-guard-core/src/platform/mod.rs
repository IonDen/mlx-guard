//! Compile-time platform selection. Darwin implementation details stay below this boundary.

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(not(target_os = "macos"))]
mod unsupported;

/// Native observation capability selected for this build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformSupport {
    /// Darwin capability code is compiled in.
    Darwin,
    /// This host intentionally has no enforcement adapter.
    Unsupported,
}

/// Return the adapter selected by the current compilation target.
#[must_use]
pub const fn platform_support() -> PlatformSupport {
    #[cfg(target_os = "macos")]
    return darwin::platform_support();

    #[cfg(not(target_os = "macos"))]
    return unsupported::platform_support();
}
