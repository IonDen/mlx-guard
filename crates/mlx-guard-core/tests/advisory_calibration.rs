use std::time::Duration;

use mlx_guard_core::{
    AdvisoryFreshness, AdvisoryScope, AdvisorySnapshot, AdvisorySource, CalibrationGuidance,
    FootprintSample, MemoryPressureLevel, ObservationError, ObserveCalibration, Observed,
    PrelaunchSummary, PrelaunchWarning, SampleOutcome, UnavailableReason,
};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn complete_sample(sequence: u64, at_ms: u64, total_bytes: u64) -> FootprintSample {
    FootprintSample {
        sequence,
        started_at: ms(at_ms - 1),
        finished_at: ms(at_ms),
        members: Vec::new(),
        outcome: SampleOutcome::Complete { total_bytes },
        events: Vec::new(),
        escape_observed: false,
    }
}

fn partial_sample(sequence: u64, at_ms: u64, known_bytes: u64) -> FootprintSample {
    FootprintSample {
        sequence,
        started_at: ms(at_ms - 1),
        finished_at: ms(at_ms),
        members: Vec::new(),
        outcome: SampleOutcome::Partial {
            known_bytes,
            missing_identities: Vec::new(),
            observation_failures: Vec::new(),
        },
        events: Vec::new(),
        escape_observed: false,
    }
}

fn system_snapshot(at_ms: u64, pressure: Observed<MemoryPressureLevel>) -> AdvisorySnapshot {
    AdvisorySnapshot::new(
        ms(at_ms),
        Observed::Unknown,
        pressure,
        Observed::Available { value: 4_096 },
        Observed::Available { value: 8_192 },
        Observed::Available { value: 16_384 },
    )
}

#[test]
fn the_calibration_peak_survives_beyond_the_report_sample_ring() {
    // Catches deriving the calibration peak from the bounded (4,096-sample) report ring instead of
    // an unbounded running max: the peak lands in the first sample, then every later one is smaller,
    // so a ring-derived peak would evict and lose it once the ring wraps. This is the property that
    // makes the emitted calibration section worth more than a `jq` maximum over `.samples[]`.
    let mut calibration = ObserveCalibration::new();
    let peak = 1_000_000;
    let _ = calibration.record_sample(
        &complete_sample(0, 1, peak),
        ms(2),
        &system_snapshot(2, Observed::Unknown),
    );
    let total = mlx_guard_core::MAX_SAMPLE_HISTORY_CAPACITY as u64 + 100;
    for sequence in 1..total {
        let at_ms = sequence + 1;
        let _ = calibration.record_sample(
            &complete_sample(sequence, at_ms, 1),
            ms(at_ms + 1),
            &system_snapshot(at_ms + 1, Observed::Unknown),
        );
    }
    let artifact = calibration.artifact();
    assert_eq!(artifact.total_samples, total);
    assert_eq!(artifact.complete_samples, total);
    assert_eq!(
        artifact.peak_aggregate_footprint_bytes,
        Observed::Available { value: peak }
    );
}

#[test]
fn observe_path_uses_sampler_windows_and_advisory_metrics_without_policy_actions() {
    // Catches a separate observe data path or advisory value becoming an enforcement input.
    let mut calibration = ObserveCalibration::new();
    let first = calibration.record_sample(
        &complete_sample(0, 10, 100),
        ms(11),
        &system_snapshot(11, Observed::Unknown),
    );
    assert_eq!(
        first.aggregate_footprint_bytes,
        Observed::Available { value: 100 }
    );
    assert_eq!(first.advisory.growth_bytes_per_second, Observed::Unknown);

    let second = calibration.record_sample(
        &complete_sample(1, 20, 200),
        ms(21),
        &system_snapshot(
            21,
            Observed::Available {
                value: MemoryPressureLevel::Critical,
            },
        ),
    );
    assert_eq!(
        second.advisory.growth_bytes_per_second,
        Observed::Available { value: 10_000 }
    );
    assert_eq!(
        second.advisory.pressure_level,
        Some(Observed::Available {
            value: MemoryPressureLevel::Critical,
        })
    );
    let metadata = second.advisory.metadata.as_ref().unwrap();
    assert_eq!(metadata.swap_bytes.scope, AdvisoryScope::System);
    assert_eq!(
        metadata.swap_bytes.source,
        AdvisorySource::SysctlVmSwapusage
    );
    assert_eq!(metadata.swap_bytes.freshness, AdvisoryFreshness::Fresh);
    assert_eq!(
        metadata.growth_bytes_per_second.scope,
        AdvisoryScope::OwnedProcessGroup
    );
    assert_eq!(
        metadata.growth_bytes_per_second.source,
        AdvisorySource::DerivedFootprintSamples
    );
    assert_eq!(calibration.intervention_count(), 0);
}

#[test]
fn sub_millisecond_native_window_remains_positive_in_the_millisecond_schema() {
    // Catches truncating a real nonzero native sample window into schema-invalid zero milliseconds.
    let sample = FootprintSample {
        sequence: 0,
        started_at: Duration::from_micros(10_000),
        finished_at: Duration::from_micros(10_125),
        members: Vec::new(),
        outcome: SampleOutcome::Complete { total_bytes: 100 },
        events: Vec::new(),
        escape_observed: false,
    };
    let mut calibration = ObserveCalibration::new();

    let projected = calibration.record_sample(
        &sample,
        Duration::from_micros(10_250),
        &system_snapshot(10, Observed::Unknown),
    );

    assert_eq!(projected.window_ms, 1);
}

#[test]
fn initial_and_missing_observations_never_become_normal_or_zero() {
    // Catches unknown pressure or a failed system query being serialized as reassuring zeroes.
    let snapshot = AdvisorySnapshot::new(
        ms(5),
        Observed::Unknown,
        Observed::Unknown,
        Observed::Unavailable {
            reason: UnavailableReason::NotSupported,
        },
        Observed::Error {
            code: ObservationError::PermissionDenied,
        },
        Observed::Unknown,
    );
    let metrics = snapshot.with_growth(&Observed::Unknown, None);
    assert_eq!(metrics.pressure_events, Observed::Unknown);
    assert_eq!(metrics.pressure_level, Some(Observed::Unknown));
    assert_eq!(
        metrics.swap_bytes,
        Observed::Unavailable {
            reason: UnavailableReason::NotSupported,
        }
    );
    assert_eq!(
        metrics.compressor_bytes,
        Observed::Error {
            code: ObservationError::PermissionDenied,
        }
    );
    let metadata = metrics.metadata.unwrap();
    assert_eq!(
        metadata.pressure_events.freshness,
        AdvisoryFreshness::InitialUnknown
    );
    assert_eq!(
        metadata.growth_bytes_per_second.freshness,
        AdvisoryFreshness::InitialUnknown
    );
}

#[test]
fn incomplete_sample_makes_growth_stale_instead_of_reusing_it_as_current() {
    // Catches carrying a previous rate across a partial sample as if it were fresh.
    let mut calibration = ObserveCalibration::new();
    let _ = calibration.record_sample(
        &complete_sample(0, 10, 100),
        ms(11),
        &system_snapshot(11, Observed::Unknown),
    );
    let _ = calibration.record_sample(
        &complete_sample(1, 20, 200),
        ms(21),
        &system_snapshot(21, Observed::Unknown),
    );
    let partial = calibration.record_sample(
        &partial_sample(2, 30, 250),
        ms(31),
        &system_snapshot(31, Observed::Unknown),
    );
    assert_eq!(
        partial.advisory.growth_bytes_per_second,
        Observed::Stale {
            last_seen_at_ms: 20
        }
    );
    assert_eq!(
        partial
            .advisory
            .metadata
            .unwrap()
            .growth_bytes_per_second
            .freshness,
        AdvisoryFreshness::Stale
    );
}

#[test]
fn growth_is_signed_and_clock_or_numeric_overflow_stays_typed() {
    // Catches unsigned subtraction, divide-by-zero, and saturating an unusable rate into a number.
    let mut calibration = ObserveCalibration::new();
    let _ = calibration.record_sample(
        &complete_sample(0, 10, 200),
        ms(11),
        &system_snapshot(11, Observed::Unknown),
    );
    let falling = calibration.record_sample(
        &complete_sample(1, 20, 100),
        ms(21),
        &system_snapshot(21, Observed::Unknown),
    );
    assert_eq!(
        falling.advisory.growth_bytes_per_second,
        Observed::Available { value: -10_000 }
    );
    let same_time = calibration.record_sample(
        &complete_sample(2, 20, 300),
        ms(21),
        &system_snapshot(21, Observed::Unknown),
    );
    assert_eq!(
        same_time.advisory.growth_bytes_per_second,
        Observed::Error {
            code: ObservationError::ClockAnomaly,
        }
    );

    let mut overflow = ObserveCalibration::new();
    let _ = overflow.record_sample(
        &complete_sample(0, 10, 0),
        ms(11),
        &system_snapshot(11, Observed::Unknown),
    );
    let overflowed = overflow.record_sample(
        &complete_sample(1, 11, u64::MAX),
        ms(12),
        &system_snapshot(12, Observed::Unknown),
    );
    assert_eq!(
        overflowed.advisory.growth_bytes_per_second,
        Observed::Error {
            code: ObservationError::Internal,
        }
    );
}

#[test]
fn prelaunch_summary_warns_without_rejecting_or_inventing_pressure_state() {
    // Catches treating an absent first pressure event as normal or as an automatic launch veto.
    let unknown = PrelaunchSummary::new(
        Observed::Available { value: true },
        &system_snapshot(0, Observed::Unknown),
    );
    assert!(
        unknown
            .warnings
            .contains(&PrelaunchWarning::PressureStateUnknown)
    );
    assert!(!unknown.reject_launch);

    let critical = PrelaunchSummary::new(
        Observed::Available { value: true },
        &system_snapshot(
            0,
            Observed::Available {
                value: MemoryPressureLevel::Critical,
            },
        ),
    );
    assert!(
        critical
            .warnings
            .contains(&PrelaunchWarning::MemoryPressureCritical)
    );
    assert!(!critical.reject_launch);
}

#[test]
fn calibration_artifact_reports_evidence_but_never_selects_a_limit_or_certifies_safety() {
    // Catches turning one safe-workload observation into a universal automatic limit.
    let mut calibration = ObserveCalibration::new();
    let _ = calibration.record_sample(
        &complete_sample(0, 10, 100),
        ms(11),
        &system_snapshot(11, Observed::Unknown),
    );
    let _ = calibration.record_sample(
        &complete_sample(1, 20, 250),
        ms(21),
        &system_snapshot(21, Observed::Unknown),
    );
    let _ = calibration.record_sample(
        &partial_sample(2, 30, 300),
        ms(31),
        &system_snapshot(31, Observed::Unknown),
    );

    let artifact = calibration.artifact();
    assert!(artifact.observation_only);
    assert!(!artifact.safety_certified);
    assert_eq!(artifact.complete_samples, 2);
    assert_eq!(artifact.incomplete_samples, 1);
    assert_eq!(
        artifact.peak_aggregate_footprint_bytes,
        Observed::Available { value: 250 }
    );
    assert_eq!(artifact.automatic_limit_bytes, None);
    assert_eq!(
        artifact.guidance,
        CalibrationGuidance::ChooseExplicitLimitFromRepeatedRepresentativeRuns
    );
    let encoded = artifact.to_json_pretty().unwrap();
    assert!(encoded.contains("\"observation_only\": true"));
    assert!(encoded.contains("\"safety_certified\": false"));
    assert!(encoded.contains("\"automatic_limit_bytes\": null"));
    assert!(!encoded.contains("command"));
    assert_eq!(
        mlx_guard_core::CalibrationArtifact::from_json(&encoded).unwrap(),
        artifact
    );
    let mut invalid = artifact;
    invalid.safety_certified = true;
    assert!(invalid.to_json_pretty().is_err());
}
