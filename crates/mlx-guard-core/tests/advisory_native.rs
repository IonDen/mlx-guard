#![cfg(unix)]

use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

#[cfg(not(target_os = "macos"))]
use mlx_guard_core::UnavailableReason;
#[cfg(target_os = "macos")]
use mlx_guard_core::{AdvisoryFreshness, AdvisorySource, ObservationError};
use mlx_guard_core::{NativeAdvisoryObserver, Observed, memory_categories};

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

#[cfg(target_os = "macos")]
fn hw_memsize() -> u64 {
    let output = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .unwrap();
    assert!(output.status.success(), "sysctl hw.memsize failed");
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
#[cfg(target_os = "macos")]
fn memory_categories_read_a_plausible_number_on_this_host() {
    // Catches counts multiplied by the wrong page size, read at the wrong offset from a stale
    // struct, or an available reading of 0: the five counts are disjoint page queues, so their sum
    // stays below physical memory, and a live Mac always has pages on them.
    let Observed::Available { value: categories } = memory_categories() else {
        panic!("memory categories unavailable: {:?}", memory_categories());
    };
    let memsize = hw_memsize();
    let estimate = categories.estimate();
    eprintln!(
        "free_bytes={} active_bytes={} inactive_bytes={} wired_bytes={} compressor_bytes={} \
         estimate={estimate} hw.memsize={memsize}",
        categories.free_bytes,
        categories.active_bytes,
        categories.inactive_bytes,
        categories.wired_bytes,
        categories.compressor_bytes,
    );
    assert!(estimate > 0);
    assert!(categories.wired_bytes > 0);
    let sum = [
        categories.free_bytes,
        categories.active_bytes,
        categories.inactive_bytes,
        categories.wired_bytes,
        categories.compressor_bytes,
    ]
    .iter()
    .try_fold(0_u64, |total, value| total.checked_add(*value))
    .unwrap();
    assert!(sum < memsize, "sum {sum} is not below hw.memsize {memsize}");
}

#[test]
#[cfg(not(target_os = "macos"))]
fn memory_categories_are_typed_unavailable_off_macos() {
    // Catches a non-macOS arm that reports zeros, which would read as no room at all.
    assert_eq!(
        memory_categories(),
        Observed::Unavailable {
            reason: UnavailableReason::NotSupported,
        }
    );
}
