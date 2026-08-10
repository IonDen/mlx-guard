use mlx_guard_core::{PlatformSupport, platform_support};

#[test]
fn current_build_uses_the_expected_platform_adapter() {
    // Catches compiling the Darwin adapter on another OS or silently disabling it on macOS.
    #[cfg(target_os = "macos")]
    assert_eq!(platform_support(), PlatformSupport::Darwin);

    #[cfg(not(target_os = "macos"))]
    assert_eq!(platform_support(), PlatformSupport::Unsupported);
}

#[test]
fn crate_version_comes_from_the_workspace_package_version() {
    // Catches a second hand-maintained version constant drifting from Cargo metadata.
    assert_eq!(mlx_guard_core::VERSION, env!("CARGO_PKG_VERSION"));
}
