use std::ffi::OsString;
use std::time::Duration;

use mlx_guard_core::{
    AdvisoryMetrics, AdvisoryScope, AdvisorySnapshot, ArtifactErrorCode, ArtifactErrorRecord,
    Capabilities, CheckpointRecord, CheckpointStatus, ChildStatus, EscapeEvidence,
    MemoryPressureLevel, ObservationError, Observed, PolicyState, PrivacyDefaults,
    REPORT_SCHEMA_VERSION, ReportConfiguration, ReportError, ReportMode, ReportV1, RunIdentity,
    SampleWindow, SignalReason, SignalRecord, SignalResult, SignalTarget, TerminalKind,
    TerminalOutcome, TransitionRecord, UnavailableReason,
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
                pressure_level: None,
                metadata: None,
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
            reason: None,
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
            child_status: None,
            owned_group_survivors: None,
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
fn validator_rejects_each_unsafe_field_through_its_own_check() {
    // Catches callers bypassing defaults or smuggling sensitive strings through correlation data,
    // and catches a validator whose checks are miswired so a bad field is rejected for the wrong
    // reason (or accepted once the check that happened to catch it changes).
    type Corrupt = fn(&mut ReportV1);
    type Expected = fn(&ReportError) -> bool;
    let cases: [(&str, Corrupt, Expected); 12] = [
        (
            "unredacted persistence",
            |r| r.privacy.redacted_before_persistence = false,
            |e| matches!(e, ReportError::InvalidPrivacy),
        ),
        (
            "non-hash correlation text",
            |r| r.run.correlation_hash = Some("SECRET_CANARY".to_owned()),
            |e| matches!(e, ReportError::InvalidIdentity),
        ),
        (
            "non-hex run id",
            |r| r.run.run_id = "SECRET_CANARY".to_owned(),
            |e| matches!(e, ReportError::InvalidIdentity),
        ),
        (
            "free-text package version",
            |r| r.package_version = "SECRET_CANARY".to_owned(),
            |e| matches!(e, ReportError::InvalidPackageVersion),
        ),
        (
            "signal number zero",
            |r| r.signals[0].signal = 0,
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "outcome before its last event",
            |r| r.outcome.at_ms = 9,
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "zero wall time",
            |r| r.configuration.wall_time_ms = Some(0),
            |e| matches!(e, ReportError::InvalidConfiguration),
        ),
        (
            "warning threshold not below the limit",
            |r| r.configuration.warning_footprint_bytes = Some(100),
            |e| matches!(e, ReportError::InvalidConfiguration),
        ),
        (
            "zero sample window",
            |r| r.samples[0].window_ms = 0,
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "child status signal out of range",
            |r| r.outcome.child_status = Some(ChildStatus::Signaled { signal: 200 }),
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "child status disagrees with the outcome kind",
            |r| {
                r.outcome.kind = TerminalKind::ChildExited { code: 0 };
                r.outcome.child_status = Some(ChildStatus::Exited { code: 3 });
            },
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "child status disagrees with a signaled outcome",
            |r| {
                r.outcome.kind = TerminalKind::ChildSignaled { signal: 9 };
                r.outcome.child_status = Some(ChildStatus::Signaled { signal: 15 });
            },
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
    ];
    for (case, corrupt, expected) in cases {
        let mut invalid = report();
        corrupt(&mut invalid);
        let error = invalid
            .validate()
            .expect_err(&format!("report with {case} must be rejected"));
        assert!(
            expected(&error),
            "report with {case} was rejected by the wrong check: {error:?}"
        );
    }
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
fn additive_advisory_metadata_is_compatible_but_cross_field_mismatches_are_rejected() {
    // Catches accepting advisory scope/source/freshness labels unrelated to their actual values.
    let snapshot = AdvisorySnapshot::new(
        Duration::from_millis(10),
        Observed::Available { value: 1 },
        Observed::Available {
            value: MemoryPressureLevel::Normal,
        },
        Observed::Available { value: 2 },
        Observed::Available { value: 3 },
        Observed::Available { value: 4 },
    );
    let mut enriched = report();
    enriched.samples[0].advisory = snapshot.with_growth(
        &Observed::Available { value: 20 },
        Some(Duration::from_millis(10)),
    );
    assert!(enriched.validate().is_ok());
    let encoded = enriched.to_json_pretty().unwrap();
    assert!(encoded.contains("\"source\": \"host_statistics64\""));

    enriched.samples[0]
        .advisory
        .metadata
        .as_mut()
        .unwrap()
        .wired_bytes
        .scope = AdvisoryScope::OwnedProcessGroup;
    assert!(matches!(
        enriched.validate(),
        Err(ReportError::InvalidAdvisoryMetrics)
    ));
}

#[test]
fn schema_v1_json_matches_the_committed_golden() {
    // Catches accidental field, enum, ordering, or privacy-default compatibility changes.
    assert_eq!(
        report().to_json_pretty().unwrap(),
        include_str!("fixtures/report-v1.json")
    );
}

#[test]
fn a_pre_0_2_report_parses_with_the_new_optional_fields_absent() {
    // Catches a new field that is not optional or not defaulted on deserialization.
    let report = ReportV1::from_json(include_str!("fixtures/report-v1.json")).unwrap();
    assert_eq!(report.outcome.child_status, None);
    assert_eq!(report.outcome.owned_group_survivors, None);
    assert!(report.signals.iter().all(|signal| signal.reason.is_none()));
    assert!(!report.to_json_pretty().unwrap().contains("child_status"));
}

#[test]
fn awkward_exit_fields_round_trip_byte_exact() {
    // Catches dropping child_status, owned_group_survivors, or a signal reason on either side of
    // JSON serialization. This is a schema-level round trip; it does not exercise
    // `From<RootOutcome> for ChildStatus` (see `report::tests` for that direct coverage).
    let json = include_str!("fixtures/report-v1-awkward.json");
    let report = ReportV1::from_json(json).unwrap();
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 23 })
    );
    assert_eq!(report.outcome.owned_group_survivors, Some(true));
    assert_eq!(
        report.signals[0].reason,
        Some(SignalReason::RootExitCleanup)
    );
    assert_eq!(report.to_json_pretty().unwrap(), json);
}
