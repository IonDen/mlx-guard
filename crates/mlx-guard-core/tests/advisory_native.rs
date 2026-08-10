#![cfg(unix)]

use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

#[cfg(not(target_os = "macos"))]
use mlx_guard_core::UnavailableReason;
#[cfg(target_os = "macos")]
use mlx_guard_core::{AdvisoryFreshness, AdvisorySource, ObservationError};
use mlx_guard_core::{NativeAdvisoryObserver, Observed};

#[test]
#[cfg(target_os = "macos")]
fn public_darwin_system_metrics_are_available_without_fabricating_pressure() {
    // Catches wiring private/unsupported counters or turning an absent first event into normal.
    let observer = NativeAdvisoryObserver::new();
    let snapshot = observer.snapshot(Duration::ZERO, Duration::from_secs(5));
    let metrics = snapshot.with_growth(&Observed::Unknown, None);
    assert!(
        matches!(
            metrics.swap_bytes,
            Observed::Available { .. }
                | Observed::Error {
                    code: ObservationError::PermissionDenied,
                }
        ),
        "unexpected swap observation: {:?}",
        metrics.swap_bytes
    );
    assert!(matches!(
        metrics.compressor_bytes,
        Observed::Available { .. }
    ));
    assert!(matches!(
        metrics.wired_bytes,
        Observed::Available { value } if value > 0
    ));
    assert!(!matches!(
        metrics.pressure_events,
        Observed::Available { value: 0 }
    ));
    let pressure_level = metrics.pressure_level.as_ref().unwrap();
    assert!(matches!(
        pressure_level,
        Observed::Unknown | Observed::Available { .. } | Observed::Stale { .. }
    ));
    let metadata = metrics.metadata.unwrap();
    assert_eq!(
        metadata.swap_bytes.source,
        AdvisorySource::SysctlVmSwapusage
    );
    assert_eq!(
        metadata.compressor_bytes.source,
        AdvisorySource::HostStatistics64
    );
    assert_eq!(metadata.wired_bytes.freshness, AdvisoryFreshness::Fresh);
}

#[test]
#[cfg(target_os = "macos")]
fn advisory_snapshot_p95_stays_inside_one_minimum_sampling_window() {
    // Catches command spawning, full-process scans, or other unbounded work in advisory sampling.
    let observer = NativeAdvisoryObserver::new();
    let mut durations = Vec::with_capacity(256);
    for index in 0..256_u64 {
        let started = Instant::now();
        let _ = observer.snapshot(Duration::from_millis(index), Duration::from_secs(5));
        durations.push(started.elapsed());
    }
    durations.sort_unstable();
    let p95 = durations[durations.len() * 95 / 100];
    eprintln!("advisory snapshot p95: {p95:?}");
    assert!(p95 <= Duration::from_millis(10), "p95={p95:?}");
}

#[test]
#[cfg(not(target_os = "macos"))]
fn unsupported_platform_returns_typed_unavailability() {
    let observer = NativeAdvisoryObserver::new();
    let metrics = observer
        .snapshot(Duration::ZERO, Duration::from_secs(5))
        .with_growth(&Observed::Unknown, None);
    assert_eq!(
        metrics.swap_bytes,
        Observed::Unavailable {
            reason: UnavailableReason::NotSupported,
        }
    );
    assert_eq!(
        metrics.pressure_events,
        Observed::Unavailable {
            reason: UnavailableReason::NotSupported,
        }
    );
}
