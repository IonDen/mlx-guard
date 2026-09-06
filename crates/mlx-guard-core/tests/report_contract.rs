use std::ffi::OsString;
use std::time::Duration;

use mlx_guard_core::{
    AdvisoryMetrics, AdvisoryScope, AdvisorySnapshot, ArtifactErrorCode, ArtifactErrorRecord,
    ArtifactKind, CALIBRATION_SCHEMA_VERSION, CalibrationArtifact, CalibrationGuidance,
    Capabilities, CheckpointArtifactRecord, CheckpointRecord, CheckpointStatus, ChildStatus,
    EscapeEvidence, MemoryPressureLevel, ObservationError, Observed, OnParentExit, ParentWatch,
    PolicyState, PrivacyDefaults, REPORT_SCHEMA_VERSION, ReportConfiguration, ReportError,
    ReportMode, ReportV1, RunIdentity, SampleWindow, SignalReason, SignalRecord, SignalResult,
    SignalTarget, TerminalKind, TerminalOutcome, TransitionRecord, UnavailableReason,
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
            on_parent_exit: None,
            parent_watch: None,
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
            request_id: None,
            reason: None,
            artifact: None,
        },
        escape: EscapeEvidence {
            detected: Observed::Available { value: false },
            escaped_count: None,
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
            parent_exited_at_ms: None,
        },
        calibration: None,
        privacy,
    }
}

fn valid_calibration_artifact() -> CalibrationArtifact {
    CalibrationArtifact {
        schema_version: CALIBRATION_SCHEMA_VERSION,
        observation_only: true,
        safety_certified: false,
        total_samples: 3,
        complete_samples: 2,
        incomplete_samples: 1,
        observed_duration_ms: 100,
        peak_aggregate_footprint_bytes: Observed::Available { value: 4096 },
        peak_growth_bytes_per_second: Observed::Unknown,
        automatic_limit_bytes: None,
        guidance: CalibrationGuidance::ChooseExplicitLimitFromRepeatedRepresentativeRuns,
    }
}

fn observe_report() -> ReportV1 {
    let mut report = report();
    report.configuration.mode = ReportMode::Observe;
    report.configuration.max_footprint_bytes = None;
    report.configuration.warning_footprint_bytes = None;
    report.configuration.recovery_footprint_bytes = None;
    report.configuration.emergency_footprint_bytes = None;
    report.configuration.wall_time_ms = None;
    report.configuration.checkpoint_timeout_ms = None;
    report
}

#[test]
fn an_enforce_report_rejects_a_calibration_section() {
    // Catches emitting the observe-only calibration section on an enforcing run: the section
    // states observation_only, so a run that enforced a limit must never carry one.
    let mut enforcing = report();
    assert!(enforcing.validate().is_ok());
    enforcing.calibration = Some(valid_calibration_artifact());
    assert!(matches!(
        enforcing.validate(),
        Err(ReportError::InvalidCalibration)
    ));
}

#[test]
fn an_observe_report_accepts_a_valid_calibration_section() {
    // Catches the validator refusing the legitimate observe calibration section.
    let mut observing = observe_report();
    assert!(
        observing.validate().is_ok(),
        "the base observe report must validate before adding calibration"
    );
    observing.calibration = Some(valid_calibration_artifact());
    assert!(observing.validate().is_ok());
}

#[test]
fn a_report_rejects_an_internally_inconsistent_calibration_section() {
    // Catches the report validator failing to run the artifact's own invariant checks: an observe
    // artifact never certifies safety, and the report must refuse one that claims it does.
    let mut observing = observe_report();
    let mut artifact = valid_calibration_artifact();
    artifact.safety_certified = true;
    observing.calibration = Some(artifact);
    assert!(matches!(
        observing.validate(),
        Err(ReportError::InvalidCalibration)
    ));
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
#[allow(clippy::too_many_lines)] // Keeping the evidence matrix linear makes omissions auditable.
fn validator_rejects_each_unsafe_field_through_its_own_check() {
    // Catches callers bypassing defaults or smuggling sensitive strings through correlation data,
    // and catches a validator whose checks are miswired so a bad field is rejected for the wrong
    // reason (or accepted once the check that happened to catch it changes).
    type Corrupt = fn(&mut ReportV1);
    type Expected = fn(&ReportError) -> bool;
    let cases: [(&str, Corrupt, Expected); 24] = [
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
        (
            "parent watch detach without the detach option",
            |r| r.configuration.parent_watch = Some(ParentWatch::Detach),
            |e| matches!(e, ReportError::InvalidConfiguration),
        ),
        (
            "detach option paired with an active parent watch",
            |r| {
                r.configuration.on_parent_exit = Some(OnParentExit::Detach);
                r.configuration.parent_watch = Some(ParentWatch::Active);
            },
            |e| matches!(e, ReportError::InvalidConfiguration),
        ),
        (
            "detach option paired with an absent parent watch",
            |r| r.configuration.on_parent_exit = Some(OnParentExit::Detach),
            |e| matches!(e, ReportError::InvalidConfiguration),
        ),
        (
            "parent exit evidence without an active or detach watch",
            |r| r.outcome.parent_exited_at_ms = Some(1),
            |e| matches!(e, ReportError::InvalidConfiguration),
        ),
        (
            "parent exit evidence after the outcome",
            |r| {
                r.configuration.parent_watch = Some(ParentWatch::Active);
                r.outcome.parent_exited_at_ms = Some(r.outcome.at_ms + 1);
            },
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "escaped count positive without an observed detection",
            |r| {
                r.escape.detected = Observed::Available { value: false };
                r.escape.escaped_count = Some(3);
            },
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "escaped count present but zero",
            |r| {
                r.escape.detected = Observed::Available { value: true };
                r.escape.escaped_count = Some(0);
            },
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "checkpoint request id zero",
            |r| r.checkpoint.request_id = Some(0),
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            // at_ms must stay None here: the golden base carries at_ms: Some(12), and the
            // pre-existing NotNegotiated => at_ms-None rule (report.rs:638) would reject this row
            // before the new request_id-status rule ever runs, proving nothing about 0063.
            "checkpoint request id present with a not-negotiated status",
            |r| {
                r.checkpoint.status = CheckpointStatus::NotNegotiated;
                r.checkpoint.at_ms = None;
                r.checkpoint.request_id = Some(7);
            },
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "checkpoint request id present with a cancelled status",
            |r| {
                r.checkpoint.status = CheckpointStatus::Cancelled;
                r.checkpoint.request_id = Some(7);
            },
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "checkpoint artifact present without an acknowledged status",
            |r| {
                r.checkpoint.artifact = Some(CheckpointArtifactRecord {
                    kind: ArtifactKind::File,
                    size_bytes: None,
                });
            },
            |e| matches!(e, ReportError::InvalidEventOrder),
        ),
        (
            "checkpoint reason outside the requestable causes",
            |r| r.checkpoint.reason = Some(SignalReason::ParentExit),
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
fn a_not_negotiated_checkpoint_with_only_a_reason_is_accepted() {
    // Catches a validator arm that rejects the commonest real report shape: an enforcing run with
    // no cooperative worker, where a checkpoint actuation was attempted toward a non-negotiated
    // channel and only the cause is latched. The table's driver only asserts rejection, so this
    // acceptance case is its own test (plan review D2).
    let mut accepted = report();
    accepted.checkpoint.status = CheckpointStatus::NotNegotiated;
    accepted.checkpoint.at_ms = None;
    accepted.checkpoint.reason = Some(SignalReason::Footprint);
    accepted.checkpoint.request_id = None;
    assert!(accepted.validate().is_ok());
}

#[test]
fn schema_v1_resume_fixture_parses_the_acknowledged_checkpoint_shape() {
    // Catches request_id, reason, or artifact being dropped or rejected on the acknowledged path.
    let report = ReportV1::from_json(include_str!("fixtures/report-v1-resume.json")).unwrap();
    assert_eq!(
        report.checkpoint.status,
        CheckpointStatus::AcknowledgedUnverifiedDurability
    );
    assert_eq!(report.checkpoint.request_id, Some(42));
    assert_eq!(report.checkpoint.reason, Some(SignalReason::Footprint));
    assert_eq!(
        report.checkpoint.artifact,
        Some(CheckpointArtifactRecord {
            kind: ArtifactKind::File,
            size_bytes: Some(1_048_576),
        })
    );
    assert_eq!(
        report.to_json_pretty().unwrap(),
        include_str!("fixtures/report-v1-resume.json")
    );
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
    assert_eq!(report.configuration.on_parent_exit, None);
    assert_eq!(report.configuration.parent_watch, None);
    assert_eq!(report.outcome.parent_exited_at_ms, None);
    assert_eq!(report.escape.escaped_count, None);
    assert!(report.signals.iter().all(|signal| signal.reason.is_none()));
    let encoded = report.to_json_pretty().unwrap();
    assert!(!encoded.contains("child_status"));
    assert!(!encoded.contains("on_parent_exit"));
    assert!(!encoded.contains("parent_watch"));
    assert!(!encoded.contains("parent_exited_at_ms"));
    assert!(!encoded.contains("escaped_count"));
}

#[test]
fn escaped_count_round_trips_when_present() {
    // Catches escaped_count being dropped or miscoded on either side of JSON serialization.
    let mut with_escapes = report();
    with_escapes.escape.detected = Observed::Available { value: true };
    with_escapes.escape.escaped_count = Some(65);
    let encoded = with_escapes.to_json_pretty().unwrap();
    assert!(encoded.contains("\"escaped_count\": 65"));
    let parsed = ReportV1::from_json(&encoded).unwrap();
    assert_eq!(parsed.escape.escaped_count, Some(65));
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

#[test]
fn parent_fields_round_trip_byte_exact() {
    // Catches dropping on_parent_exit, parent_watch, a parent_exit signal reason, or
    // parent_exited_at_ms on either side of JSON serialization.
    let json = include_str!("fixtures/report-v1-parent.json");
    let report = ReportV1::from_json(json).unwrap();
    assert_eq!(
        report.configuration.on_parent_exit,
        Some(OnParentExit::Terminate)
    );
    assert_eq!(report.configuration.parent_watch, Some(ParentWatch::Active));
    assert_eq!(report.signals[0].reason, Some(SignalReason::ParentExit));
    assert_eq!(report.outcome.parent_exited_at_ms, Some(120));
    assert_eq!(report.to_json_pretty().unwrap(), json);
}
