use std::ffi::OsString;

use mlx_guard_core::{
    AdvisoryMetrics, ArtifactErrorCode, ArtifactErrorRecord, Capabilities, CheckpointRecord,
    CheckpointStatus, EscapeEvidence, ObservationError, Observed, PolicyState, PrivacyDefaults,
    REPORT_SCHEMA_VERSION, ReportConfiguration, ReportMode, ReportV1, RunIdentity, SampleWindow,
    SignalRecord, SignalResult, SignalTarget, TerminalKind, TerminalOutcome, TransitionRecord,
    UnavailableReason,
};

fn identity() -> RunIdentity {
    RunIdentity::from_argv(
        "run_0123456789abcdef0123456789abcdef",
        &[
            OsString::from("/private/SECRET_CANARY/bin/python"),
            OsString::from("--token=SECRET_CANARY"),
        ],
        Some("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
    )
    .unwrap()
}

fn report() -> ReportV1 {
    let privacy = PrivacyDefaults {
        capture: mlx_guard_core::CapturePolicy::RedactedMetadataWithCorrelationHash,
        ..PrivacyDefaults::default()
    };
    ReportV1 {
        schema_version: REPORT_SCHEMA_VERSION,
        package_version: "0.1.0".to_owned(),
        run: identity(),
        capabilities: Capabilities {
            darwin_footprint: Observed::Available { value: true },
            owned_process_group: Observed::Available { value: true },
            checkpoint_channel: Observed::Unavailable {
                reason: UnavailableReason::NotNegotiated,
            },
        },
        configuration: ReportConfiguration {
            mode: ReportMode::Enforce,
            max_footprint_bytes: Some(100),
            warning_footprint_bytes: Some(90),
            recovery_footprint_bytes: Some(80),
            emergency_footprint_bytes: Some(150),
            required_breach_samples: 2,
            max_missing_samples: 3,
            wall_time_ms: Some(1_000),
            sample_interval_ms: 50,
            max_sample_age_ms: 100,
            max_sample_window_ms: 10,
            checkpoint_timeout_ms: Some(50),
            term_grace_ms: 100,
        },
        samples: vec![SampleWindow {
            captured_at_ms: 10,
            processed_at_ms: 11,
            window_ms: 1,
            aggregate_footprint_bytes: Observed::Available { value: 90 },
            advisory: AdvisoryMetrics {
                pressure_events: Observed::Unknown,
                swap_bytes: Observed::Stale { last_seen_at_ms: 5 },
                compressor_bytes: Observed::Unavailable {
                    reason: UnavailableReason::NotSupported,
                },
                wired_bytes: Observed::Error {
                    code: ObservationError::PermissionDenied,
                },
                growth_bytes_per_second: Observed::Available { value: 20 },
            },
        }],
        transitions: vec![TransitionRecord {
            at_ms: 12,
            from: PolicyState::Normal,
            to: PolicyState::Warning,
            aggregate_footprint_bytes: Some(100),
        }],
        signals: vec![SignalRecord {
            at_ms: 13,
            signal: 15,
            target: SignalTarget::OwnedProcessGroup,
            result: SignalResult::Delivered,
        }],
        checkpoint: CheckpointRecord {
            status: CheckpointStatus::RequestedUnverified,
            at_ms: Some(12),
        },
        escape: EscapeEvidence {
            detected: Observed::Available { value: false },
        },
        artifact_errors: vec![ArtifactErrorRecord {
            at_ms: 14,
            code: ArtifactErrorCode::SyncFailed,
        }],
        outcome: TerminalOutcome {
            at_ms: 15,
            kind: TerminalKind::PolicyIntervention,
            final_footprint_bytes: Observed::Unknown,
        },
        privacy,
    }
}

#[test]
fn report_projection_drops_secret_argv_paths_environment_and_output() {
    // Catches storing the original argv or executable path behind the redacted public fields.
    let encoded = report().to_json_pretty().unwrap();
    assert!(!encoded.contains("SECRET_CANARY"));
    assert!(!encoded.contains("/private/"));
    assert!(!encoded.contains("--token"));
    assert!(!encoded.contains("environment"));
    assert!(!encoded.contains("child_output"));
    assert!(encoded.contains("\"executable_basename\": \"python\""));
    assert!(encoded.contains("\"argument_count\": 1"));
}

#[test]
fn unavailable_unknown_stale_and_error_are_not_serialized_as_zero() {
    // Catches a compacting mutant that erases why an observation has no usable value.
    let encoded = report().to_json_pretty().unwrap();
    for status in ["unknown", "unavailable", "stale", "error"] {
        assert!(encoded.contains(&format!("\"status\": \"{status}\"")));
    }
}

#[test]
fn validator_rejects_unsafe_privacy_hash_signal_and_event_order() {
    // Catches callers bypassing defaults or smuggling sensitive strings through correlation data.
    let mut invalid = report();
    invalid.privacy.redacted_before_persistence = false;
    assert!(invalid.validate().is_err());

    let mut invalid = report();
    invalid.run.correlation_hash = Some("SECRET_CANARY".to_owned());
    assert!(invalid.validate().is_err());

    let mut invalid = report();
    invalid.run.run_id = "SECRET_CANARY".to_owned();
    assert!(invalid.validate().is_err());

    let mut invalid = report();
    invalid.package_version = "SECRET_CANARY".to_owned();
    assert!(invalid.validate().is_err());

    let mut invalid = report();
    invalid.signals[0].signal = 0;
    assert!(invalid.validate().is_err());

    let mut invalid = report();
    invalid.outcome.at_ms = 9;
    assert!(invalid.validate().is_err());

    let mut invalid = report();
    invalid.configuration.wall_time_ms = Some(0);
    assert!(invalid.validate().is_err());

    let mut invalid = report();
    invalid.configuration.warning_footprint_bytes = Some(100);
    assert!(invalid.validate().is_err());

    let mut invalid = report();
    invalid.samples[0].window_ms = 0;
    assert!(invalid.validate().is_err());
}

#[test]
fn schema_reader_accepts_extensions_but_rejects_other_major_versions() {
    // Catches strict unknown-field parsing and a version-skew mutant that guesses at schema v2.
    let encoded = report().to_json_pretty().unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    value["future_extension"] = serde_json::json!({"value": "SECRET_CANARY"});
    let parsed = ReportV1::from_json(&serde_json::to_string(&value).unwrap()).unwrap();
    assert!(!parsed.to_json_pretty().unwrap().contains("SECRET_CANARY"));

    value["schema_version"] = serde_json::json!(2);
    assert!(ReportV1::from_json(&serde_json::to_string(&value).unwrap()).is_err());
}

#[cfg(unix)]
#[test]
fn non_utf8_executable_bytes_are_not_copied_into_json() {
    // Catches lossy conversion that could persist sensitive or misleading raw path bytes.
    use std::os::unix::ffi::OsStringExt;

    let identity = RunIdentity::from_argv(
        "run_0123456789abcdef0123456789abcdef",
        &[OsString::from_vec(b"/tmp/python-\xff".to_vec())],
        None,
    )
    .unwrap();
    assert_eq!(identity.executable_basename, "<non-utf8>");
}

#[test]
fn schema_v1_json_matches_the_committed_golden() {
    // Catches accidental field, enum, ordering, or privacy-default compatibility changes.
    assert_eq!(
        report().to_json_pretty().unwrap(),
        include_str!("fixtures/report-v1.json")
    );
}
