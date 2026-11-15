//! The automatic limit: the rule, the resolver, and the two report fields that record it.
//!
//! Figures are illustrative (an 8.00 GiB estimate gives a 6.00 GiB limit), never one Mac's
//! readings. Expected values come from the written rule: `floor(B * 3 / 4)` computed without
//! overflow, rounded down to a whole MiB, and no usable number under 1 GiB. The policy band is the
//! one in `docs/POLICY.md`: step `max(1, limit / 10)`, emergency `limit + step`.

use mlx_guard_core::{
    AutoLimitRecord, AutoLimitRule, AutoLimitUnavailable, FootprintRequest, LimitSource,
    MemoryCategories, ObservationError, Observed, PrelaunchRecord, PrelaunchWarning, ReportError,
    ReportMode, ReportV1, UnavailableReason, resolve_limit,
};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// The illustrative estimate: 8.00 GiB of memory macOS did not report as wired.
const ESTIMATE: u64 = 8_589_934_592;
/// Three quarters of [`ESTIMATE`], already a whole MiB: 6.00 GiB.
const LIMIT: u64 = 6_442_450_944;
/// The band around [`LIMIT`] by `docs/POLICY.md`: step = floor(6442450944 / 10) = 644245094.
const WARNING: u64 = 5_798_205_850;
const RECOVERY: u64 = 5_153_960_756;
const EMERGENCY: u64 = 7_086_696_038;

const GOLDEN: &str = include_str!("fixtures/report-v1.json");

/// Page counts whose estimate (everything but wired) is exactly [`ESTIMATE`].
fn categories() -> MemoryCategories {
    MemoryCategories {
        free_bytes: 1_073_741_824,
        active_bytes: 3_221_225_472,
        inactive_bytes: 3_221_225_472,
        wired_bytes: 2_147_483_648,
        compressor_bytes: 1_073_741_824,
    }
}

/// Page counts whose estimate is exactly `estimate`, all of it free.
fn categories_with_estimate(estimate: u64) -> Observed<MemoryCategories> {
    Observed::Available {
        value: MemoryCategories {
            free_bytes: estimate,
            active_bytes: 0,
            inactive_bytes: 0,
            wired_bytes: 0,
            compressor_bytes: 0,
        },
    }
}

/// The rule's formula, restated step for step as the implementation computes it: `u128` share,
/// round down to a MiB, refuse under 1 GiB. Comparing against it only shows agreement with the
/// formula and the absence of overflow; the spec itself is pinned by hand values and by
/// `limit_steps_exactly_at_each_rounding_edge`.
fn formula_restated(estimate: u64) -> Option<u64> {
    let three_quarters = u128::from(estimate) * 3 / 4;
    let whole_mib = three_quarters / u128::from(MIB) * u128::from(MIB);
    let limit = u64::try_from(whole_mib).expect("three quarters of a u64 fits a u64");
    (limit >= GIB).then_some(limit)
}

/// The emergency threshold `docs/POLICY.md` derives from a limit.
fn emergency_for(limit: u64) -> Option<u64> {
    limit.checked_add((limit / 10).max(1))
}

/// Every estimate on a 1 MiB grid from 1 GiB (under the floor) to 512 GiB, each also 1 B either
/// side, plus the three end values. About 1.57 million cases.
fn sweep_estimates() -> impl Iterator<Item = u64> {
    (GIB / MIB..=512 * GIB / MIB)
        .flat_map(|mib| {
            let on_grid = mib * MIB;
            [on_grid - 1, on_grid, on_grid + 1]
        })
        .chain([u64::MAX, u64::MAX - 1, u64::MAX / 2])
}

/// A valid enforce report recording an automatic 6.00 GiB limit from an 8.00 GiB estimate.
fn auto_report() -> ReportV1 {
    let mut report = ReportV1::from_json(GOLDEN).expect("the committed golden must parse");
    report.configuration.max_footprint_bytes = Some(LIMIT);
    report.configuration.warning_footprint_bytes = Some(WARNING);
    report.configuration.recovery_footprint_bytes = Some(RECOVERY);
    report.configuration.emergency_footprint_bytes = Some(EMERGENCY);
    report.configuration.max_footprint_source = Some(LimitSource::Auto);
    report.prelaunch = Some(PrelaunchRecord {
        memory_estimate: Observed::Available { value: ESTIMATE },
        memory_categories: Observed::Available {
            value: categories(),
        },
        warnings: Vec::new(),
        auto_limit: Some(AutoLimitRecord {
            rule: AutoLimitRule::NotWired75Percent,
            limit_bytes: LIMIT,
        }),
    });
    report
}

/// The same report as an explicit run of 6.00 GiB would record it: no `auto_limit`.
fn explicit_report() -> ReportV1 {
    let mut report = auto_report();
    report.configuration.max_footprint_source = Some(LimitSource::Explicit);
    report
        .prelaunch
        .as_mut()
        .expect("the auto report has a prelaunch record")
        .auto_limit = None;
    report
}

fn auto_limit_mut(report: &mut ReportV1) -> &mut Option<AutoLimitRecord> {
    &mut report
        .prelaunch
        .as_mut()
        .expect("the auto report has a prelaunch record")
        .auto_limit
}

// ---------------------------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------------------------

#[test]
fn limit_is_three_quarters_of_the_estimate_rounded_down_to_a_mib() {
    // Bug: another share (4/5 gives 6871318528 here once rounded) or MiB rounding up instead of
    // down (the worked example below would give 21713 MiB).
    let rule = AutoLimitRule::NotWired75Percent;
    assert_eq!(AutoLimitRule::CURRENT, rule);
    assert_eq!(rule.percent(), 75);
    assert_eq!(rule.limit_for(ESTIMATE), Some(LIMIT));
    // The design's worked example: 30355927040 * 3 / 4 = 22766945280, which is 21712.25 MiB, so
    // the limit is 21712 MiB.
    assert_eq!(rule.limit_for(30_355_927_040), Some(22_766_682_112));
    // Around one MiB step above 6.00 GiB: 1398101 B of extra estimate is 1048575.75 B of extra
    // limit (still 6.00 GiB once rounded down), 1398102 B is exactly 1 MiB more.
    assert_eq!(rule.limit_for(ESTIMATE + 1_398_101), Some(LIMIT));
    assert_eq!(rule.limit_for(ESTIMATE + 1_398_102), Some(LIMIT + MIB));
}

#[test]
fn limit_agrees_with_the_formula_on_the_grid_with_no_overflow() {
    // Agrees with the formula; no overflow. Bug: a panic or a wrapped share anywhere between the
    // floor and 512 GiB or at the u64 end values, or the code drifting from its own formula. The
    // reference repeats the implementation, so the spec checks are the hand values and the
    // rounding-edge test below.
    let rule = AutoLimitRule::NotWired75Percent;
    for estimate in sweep_estimates() {
        assert_eq!(
            rule.limit_for(estimate),
            formula_restated(estimate),
            "estimate {estimate}"
        );
    }
}

/// The smallest estimate whose three quarters reach `m` MiB: `ceil(4m/3 MiB)`, in `u128` so the
/// top of the range does not overflow.
fn edge_estimate(m: u64) -> u128 {
    (4 * u128::from(m) * u128::from(MIB)).div_ceil(3)
}

/// The limit the spec gives once three quarters of the estimate have reached `m` MiB (and not yet
/// `m + 1` MiB): exactly `m` MiB, or nothing under the 1 GiB floor (1024 MiB).
fn spec_limit_at(m: u64) -> Option<u64> {
    (m >= 1024).then(|| m * MIB)
}

#[test]
fn limit_steps_exactly_at_each_rounding_edge() {
    // Bug: the limit rounded up (or to the nearest MiB) instead of down, the share computed as
    // `ceil(3B/4)` or `B / 4 * 3`, or the 1 GiB floor misplaced. Each limit edge m MiB sits at
    // estimate e = ceil(4m/3 MiB): e - 1 must still give m - 1 MiB, e and e + 1 give m MiB. Only a
    // third of these edges lie on the MiB grid the sweep above probes.
    let rule = AutoLimitRule::NotWired75Percent;
    // Every edge from 1 MiB to 64 GiB of limit (65536 MiB), which crosses the floor at 1024 MiB,
    // plus the last three edges under u64::MAX (the largest m with e in range is 3 * 2^42 - 1).
    let top = 3 * (1_u64 << 42);
    for m in (1..=65_536).chain(top - 3..top) {
        let edge = u64::try_from(edge_estimate(m)).expect("the edge estimate fits a u64");
        assert_eq!(
            rule.limit_for(edge - 1),
            spec_limit_at(m - 1),
            "m {m}: one byte under the edge"
        );
        assert_eq!(rule.limit_for(edge), spec_limit_at(m), "m {m}: on the edge");
        assert_eq!(
            rule.limit_for(edge + 1),
            spec_limit_at(m),
            "m {m}: one byte over the edge"
        );
    }
    // Hand-checked anchors for the helper itself: the floor edge is the 1431655766 of
    // `estimate_at_the_floor_boundary`, and 6.00 GiB + 1 MiB is the 8.00 GiB + 1398102 above.
    assert_eq!(edge_estimate(1024), 1_431_655_766);
    assert_eq!(edge_estimate(6145), u128::from(ESTIMATE + 1_398_102));
}

#[test]
fn emergency_stays_at_or_under_the_estimate_for_every_estimate() {
    // Bug: a rule too generous for its band, so the emergency KILL threshold (limit + limit / 10)
    // sits above the memory macOS reported as not wired.
    let rule = AutoLimitRule::NotWired75Percent;
    let mut usable = 0_u64;
    for estimate in sweep_estimates() {
        if let Some(limit) = rule.limit_for(estimate) {
            usable += 1;
            let emergency = emergency_for(limit).expect("the emergency threshold fits a u64");
            assert!(
                emergency <= estimate,
                "estimate {estimate}: limit {limit}, emergency {emergency}"
            );
        }
    }
    // Guards the sweep itself: a rule that never returns a number would pass the loop above.
    assert!(usable > 500_000, "only {usable} estimates gave a limit");
}

#[test]
fn estimate_at_the_floor_boundary() {
    // Bug: the floor compared with `<=` (a limit of exactly 1 GiB refused), the floor skipped
    // (1 GiB - 1 MiB accepted), or `B / 4 * 3` instead of `B * 3 / 4` (1431655766 gives
    // 1073741823, so 1 GiB is lost).
    let rule = AutoLimitRule::NotWired75Percent;
    // 2^32 / 3 = 1431655765.33: the smallest estimate whose three quarters reach 1 GiB is
    // 1431655766. One byte less gives 1073741823.75, which rounds down to 1 GiB - 1 MiB.
    assert_eq!(rule.limit_for(1_431_655_765), None);
    assert_eq!(rule.limit_for(1_431_655_766), Some(GIB));
    assert_eq!(rule.limit_for(1_431_655_767), Some(GIB));

    assert!(matches!(
        resolve_limit(
            FootprintRequest::Auto,
            categories_with_estimate(1_431_655_765)
        ),
        Err(AutoLimitUnavailable::BelowFloor)
    ));
    match resolve_limit(
        FootprintRequest::Auto,
        categories_with_estimate(1_431_655_766),
    ) {
        Ok((resolved, _)) => {
            assert_eq!(resolved.limit_bytes, GIB);
            assert_eq!(resolved.source, LimitSource::Auto);
        }
        Err(error) => panic!("an estimate giving exactly 1 GiB must resolve: {error:?}"),
    }
}

#[test]
fn limit_never_overflows_near_u64_max() {
    // Bug: `B * 3` in u64, which panics in a debug build and wraps to a small limit in release.
    // 3/4 of 2^64 is 3 * 2^62 = 13835058055282163712, a whole MiB; u64::MAX is one byte short,
    // so the limit is one MiB under it. Likewise 3 * 2^61 for u64::MAX / 2.
    let rule = AutoLimitRule::NotWired75Percent;
    assert_eq!(rule.limit_for(u64::MAX), Some(13_835_058_055_281_115_136));
    assert_eq!(
        rule.limit_for(u64::MAX - 1),
        Some(13_835_058_055_281_115_136)
    );
    assert_eq!(
        rule.limit_for(u64::MAX / 2),
        Some(6_917_529_027_640_033_280)
    );
}

#[test]
fn resolved_limit_always_passes_the_cli_size_checks() {
    // Bug: a resolved limit that a typed size could not be (under 2B, or so large that
    // limit + step overflows the emergency band), so `auto` enforces a value `run` would refuse.
    // An invariant of the core side only: it restates the CLI's two checks. The CLI's own
    // `check_typed_size` is run on the largest automatic limit and on the floor in its lib.rs tests.
    for estimate in sweep_estimates() {
        if let Ok((resolved, _)) =
            resolve_limit(FootprintRequest::Auto, categories_with_estimate(estimate))
        {
            assert!(resolved.limit_bytes >= 2, "estimate {estimate}");
            assert!(
                emergency_for(resolved.limit_bytes).is_some(),
                "estimate {estimate}: limit {} overflows the band",
                resolved.limit_bytes
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The resolver
// ---------------------------------------------------------------------------------------------

#[test]
fn auto_without_an_available_estimate_is_unavailable() {
    // Bug: a missing number read as 0 (reported as "below the floor" instead of "no number"), or
    // one `Observed` variant falling through to a limit. A typed size is never refused for it.
    let missing: [Observed<MemoryCategories>; 7] = [
        Observed::Unknown,
        Observed::Unavailable {
            reason: UnavailableReason::NotSupported,
        },
        Observed::Unavailable {
            reason: UnavailableReason::NotNegotiated,
        },
        Observed::Unavailable {
            reason: UnavailableReason::NotApplicable,
        },
        Observed::Unavailable {
            reason: UnavailableReason::NonUtf8,
        },
        Observed::Stale { last_seen_at_ms: 7 },
        Observed::Error {
            code: ObservationError::Internal,
        },
    ];
    for categories in missing {
        assert!(
            matches!(
                resolve_limit(FootprintRequest::Auto, categories.clone()),
                Err(AutoLimitUnavailable::NoEstimate)
            ),
            "auto with {categories:?}"
        );
        assert!(
            resolve_limit(FootprintRequest::Bytes(LIMIT), categories.clone()).is_ok(),
            "a typed size with {categories:?}"
        );
    }
    // An available number that is too small is the other refusal, not "no number".
    assert!(matches!(
        resolve_limit(FootprintRequest::Auto, categories_with_estimate(GIB)),
        Err(AutoLimitUnavailable::BelowFloor)
    ));
}

#[test]
fn auto_request_resolves_to_the_rule_limit_and_records_it() {
    // Bug: the record names a different number than the one resolved, drops the rule, or loses
    // the estimate the 0.5.0 record keeps.
    let Ok((resolved, record)) = resolve_limit(
        FootprintRequest::Auto,
        Observed::Available {
            value: categories(),
        },
    ) else {
        panic!("an 8.00 GiB estimate must resolve");
    };
    assert_eq!(resolved.limit_bytes, LIMIT);
    assert_eq!(resolved.source, LimitSource::Auto);
    assert_eq!(
        record,
        PrelaunchRecord {
            memory_estimate: Observed::Available { value: ESTIMATE },
            memory_categories: Observed::Available {
                value: categories(),
            },
            warnings: Vec::new(),
            auto_limit: Some(AutoLimitRecord {
                rule: AutoLimitRule::NotWired75Percent,
                limit_bytes: LIMIT,
            }),
        }
    );
}

#[test]
fn explicit_request_keeps_the_0_5_2_prelaunch_record() {
    // Reference: the prelaunch record 0.5.2 writes for a typed size (0.5.2 kept 0.5.0's shape).
    // Bug: the resolver builds the explicit record its own way (a lost warning, an auto_limit on
    // an explicit run, or a typed size changed on the way through).
    let Ok((resolved, record)) = resolve_limit(
        FootprintRequest::Bytes(100_000_007),
        Observed::Available {
            value: categories(),
        },
    ) else {
        panic!("a typed size always resolves");
    };
    assert_eq!(resolved.limit_bytes, 100_000_007);
    assert_eq!(resolved.source, LimitSource::Explicit);
    assert_eq!(
        record,
        PrelaunchRecord {
            memory_estimate: Observed::Available { value: ESTIMATE },
            memory_categories: Observed::Available {
                value: categories(),
            },
            warnings: Vec::new(),
            auto_limit: None,
        }
    );

    // 16.00 GiB is more than the 8.00 GiB estimate: the 0.5.2 warning stays.
    let Ok((resolved, record)) = resolve_limit(
        FootprintRequest::Bytes(2 * ESTIMATE),
        Observed::Available {
            value: categories(),
        },
    ) else {
        panic!("a typed size always resolves");
    };
    assert_eq!(resolved.limit_bytes, 2 * ESTIMATE);
    assert_eq!(record.warnings, [PrelaunchWarning::LimitMayNotFit]);
    assert_eq!(record.auto_limit, None);

    // No number: the typed size still resolves, with the reason kept and no warning.
    let Ok((resolved, record)) = resolve_limit(FootprintRequest::Bytes(LIMIT), Observed::Unknown)
    else {
        panic!("a typed size resolves without an estimate");
    };
    assert_eq!(resolved.limit_bytes, LIMIT);
    assert_eq!(
        record,
        PrelaunchRecord {
            memory_estimate: Observed::Unknown,
            memory_categories: Observed::Unknown,
            warnings: Vec::new(),
            auto_limit: None,
        }
    );
}

// ---------------------------------------------------------------------------------------------
// The report fields
// ---------------------------------------------------------------------------------------------

#[test]
fn auto_report_round_trips() {
    // Bug: a field dropped on either side of JSON, written somewhere other than its documented
    // place (the source right after the limit, `auto_limit` after `warnings`), or a wire shape
    // other than `{rule, limit_bytes}`.
    let report = auto_report();
    let encoded = report.to_json_pretty().expect("the auto report is valid");
    assert!(
        encoded.contains(
            "\"max_footprint_bytes\": 6442450944,\n    \"max_footprint_source\": \"auto\",\n"
        ),
        "{encoded}"
    );
    assert!(
        encoded.contains(
            "\"warnings\": [],\n    \"auto_limit\": {\n      \"rule\": \"not_wired_75_percent\",\n      \"limit_bytes\": 6442450944\n    }\n  },"
        ),
        "{encoded}"
    );
    let decoded = ReportV1::from_json(&encoded).expect("the encoded auto report parses");
    assert_eq!(decoded, report);
    assert_eq!(decoded.to_json_pretty().unwrap(), encoded);
}

#[test]
fn rule_serializes_as_not_wired_75_percent() {
    // Bug: serde's snake_case on `NotWired75Percent`, which writes `not_wired75_percent`.
    assert_eq!(
        serde_json::to_string(&AutoLimitRule::NotWired75Percent).unwrap(),
        "\"not_wired_75_percent\""
    );
}

#[test]
fn known_rule_and_source_values_round_trip_byte_exact() {
    // Bug: a known value read back as the fallback (or as another value), or written in another
    // spelling than the one it was read from.
    let rule: AutoLimitRule = serde_json::from_str("\"not_wired_75_percent\"").unwrap();
    assert_eq!(rule, AutoLimitRule::NotWired75Percent);
    assert_eq!(
        serde_json::to_string(&rule).unwrap(),
        "\"not_wired_75_percent\""
    );
    for (text, source) in [
        ("\"explicit\"", LimitSource::Explicit),
        ("\"auto\"", LimitSource::Auto),
    ] {
        let read: LimitSource = serde_json::from_str(text).unwrap();
        assert_eq!(read, source);
        assert_eq!(serde_json::to_string(&read).unwrap(), text);
    }
}

#[test]
fn unknown_rule_id_is_read_without_failing() {
    // Bug: a report written by a later release with a rule id this one does not know fails to
    // parse, so the explainer and every reader of that report break.
    let encoded = auto_report().to_json_pretty().unwrap();
    let later = encoded.replace("\"not_wired_75_percent\"", "\"not_wired_70_percent\"");
    assert_ne!(later, encoded, "the substitution must hit the rule id");
    let report = ReportV1::from_json(&later).expect("an unknown rule id must be read");
    let auto_limit = report
        .prelaunch
        .and_then(|prelaunch| prelaunch.auto_limit)
        .expect("the record survives an unknown rule id");
    assert_ne!(auto_limit.rule, AutoLimitRule::NotWired75Percent);
    assert_eq!(auto_limit.limit_bytes, LIMIT);
    assert_eq!(
        report.configuration.max_footprint_source,
        Some(LimitSource::Auto)
    );
}

#[test]
fn unknown_limit_source_is_read_without_failing() {
    // Bug: a report written by a later release with a source this one does not know fails to
    // parse, or the unknown value is read as one of the known ones.
    let encoded = explicit_report().to_json_pretty().unwrap();
    let later = encoded.replace(
        "\"max_footprint_source\": \"explicit\"",
        "\"max_footprint_source\": \"a_later_source\"",
    );
    assert_ne!(later, encoded, "the substitution must hit the source");
    let report = ReportV1::from_json(&later).expect("an unknown source must be read");
    let source = report
        .configuration
        .max_footprint_source
        .expect("an unknown source is still a recorded source");
    assert_ne!(source, LimitSource::Explicit);
    assert_ne!(source, LimitSource::Auto);
}

#[test]
fn unknown_limit_source_with_an_auto_limit_record_is_read() {
    // Bug: the validator rejects an unknown source that carries an `auto_limit` record, so a
    // report from a later release whose new source also records how the limit was picked fails
    // to parse.
    let encoded = auto_report().to_json_pretty().unwrap();
    let later = encoded.replace(
        "\"max_footprint_source\": \"auto\"",
        "\"max_footprint_source\": \"a_later_source\"",
    );
    assert_ne!(later, encoded, "the substitution must hit the source");
    let report = ReportV1::from_json(&later)
        .expect("an unknown source with an auto_limit record must be read");
    assert_eq!(
        report.configuration.max_footprint_source,
        Some(LimitSource::Unknown)
    );
    assert_eq!(
        report.prelaunch.and_then(|prelaunch| prelaunch.auto_limit),
        Some(AutoLimitRecord {
            rule: AutoLimitRule::NotWired75Percent,
            limit_bytes: LIMIT,
        })
    );
}

#[test]
fn explicit_report_without_new_fields_still_validates() {
    // Bug: a new field made mandatory (0.5.0 and older reports stop parsing), read as something
    // other than absent, or written back as `null` (an old report no longer round-trips).
    let golden = ReportV1::from_json(GOLDEN).expect("the committed golden must parse");
    assert_eq!(golden.configuration.max_footprint_source, None);
    assert_eq!(golden.prelaunch, None);

    // A 0.5.0 run report: the golden plus the prelaunch record 0.5.0 wrote, with neither field.
    let with_prelaunch = GOLDEN.replace(
        "\n  \"privacy\": {",
        "\n  \"prelaunch\": {
    \"memory_estimate\": {
      \"status\": \"available\",
      \"value\": 8589934592
    },
    \"memory_categories\": {
      \"status\": \"available\",
      \"value\": {
        \"free_bytes\": 1073741824,
        \"active_bytes\": 3221225472,
        \"inactive_bytes\": 3221225472,
        \"wired_bytes\": 2147483648,
        \"compressor_bytes\": 1073741824
      }
    },
    \"warnings\": []
  },
  \"privacy\": {",
    );
    assert_ne!(
        with_prelaunch, GOLDEN,
        "the prelaunch block must be inserted"
    );
    let report = ReportV1::from_json(&with_prelaunch).expect("a 0.5.0 run report must parse");
    assert_eq!(report.configuration.max_footprint_source, None);
    assert_eq!(report.prelaunch.as_ref().unwrap().auto_limit, None);
    assert_eq!(report.to_json_pretty().unwrap(), with_prelaunch);
}

#[test]
fn golden_report_v1_is_unchanged() {
    // Bug: a new field serialized when absent (as `null` or a default), so the committed v1
    // golden no longer round-trips byte for byte.
    let report = ReportV1::from_json(GOLDEN).expect("the committed golden must parse");
    assert_eq!(report.to_json_pretty().unwrap(), GOLDEN);
}

// ---------------------------------------------------------------------------------------------
// The validator (V1-V4)
// ---------------------------------------------------------------------------------------------

#[test]
fn source_on_observe_is_rejected() {
    // Bug: V1 missing, so an observe report may claim a limit source it never had.
    let mut observe = ReportV1::from_json(GOLDEN).unwrap();
    observe.configuration.mode = ReportMode::Observe;
    observe.configuration.max_footprint_bytes = None;
    observe.configuration.warning_footprint_bytes = None;
    observe.configuration.recovery_footprint_bytes = None;
    observe.configuration.emergency_footprint_bytes = None;
    observe.configuration.wall_time_ms = None;
    observe.configuration.checkpoint_timeout_ms = None;
    assert!(
        observe.validate().is_ok(),
        "the base observe report is valid"
    );

    for source in [LimitSource::Explicit, LimitSource::Auto] {
        let mut claimed = observe.clone();
        claimed.configuration.max_footprint_source = Some(source);
        assert!(
            matches!(claimed.validate(), Err(ReportError::InvalidConfiguration)),
            "observe with source {source:?}"
        );
    }
}

#[test]
fn auto_without_auto_limit_is_rejected() {
    // Bug: V2's forward half missing, so an auto run need not say which rule chose its limit.
    assert!(
        auto_report().validate().is_ok(),
        "the base auto report is valid"
    );
    let mut report = auto_report();
    *auto_limit_mut(&mut report) = None;
    assert!(matches!(
        report.validate(),
        Err(ReportError::InvalidPrelaunch)
    ));
}

#[test]
fn auto_without_prelaunch_is_rejected() {
    // Bug: the check lives only where a prelaunch record exists, so dropping the whole record
    // hides the missing rule (the `None` branch of the prelaunch check).
    let mut report = auto_report();
    report.prelaunch = None;
    assert!(matches!(
        report.validate(),
        Err(ReportError::InvalidPrelaunch)
    ));
}

#[test]
fn auto_limit_with_explicit_source_is_rejected() {
    // Bug: V2's backward half missing, so an explicit run may carry an automatic record.
    assert!(
        explicit_report().validate().is_ok(),
        "the base explicit report is valid"
    );
    let mut report = explicit_report();
    *auto_limit_mut(&mut report) = Some(AutoLimitRecord {
        rule: AutoLimitRule::NotWired75Percent,
        limit_bytes: LIMIT,
    });
    assert!(matches!(
        report.validate(),
        Err(ReportError::InvalidPrelaunch)
    ));
}

#[test]
fn auto_limit_with_absent_source_is_rejected() {
    // Bug: an absent source (which reads as explicit) accepted beside an automatic record.
    let mut report = auto_report();
    report.configuration.max_footprint_source = None;
    assert!(matches!(
        report.validate(),
        Err(ReportError::InvalidPrelaunch)
    ));
}

#[test]
fn auto_limit_bytes_must_equal_the_enforced_limit() {
    // Bug: V3 missing, so the recorded automatic number and the enforced limit may disagree.
    for recorded in [LIMIT - 1, LIMIT + 1, LIMIT + MIB] {
        let mut report = auto_report();
        *auto_limit_mut(&mut report) = Some(AutoLimitRecord {
            rule: AutoLimitRule::NotWired75Percent,
            limit_bytes: recorded,
        });
        assert!(
            matches!(report.validate(), Err(ReportError::InvalidPrelaunch)),
            "recorded {recorded} against enforced {LIMIT}"
        );
    }
}

#[test]
fn auto_requires_an_available_estimate() {
    // Bug: V4 missing, so an automatic limit may claim to come from a number that was not there.
    let missing: [Observed<u64>; 4] = [
        Observed::Unknown,
        Observed::Unavailable {
            reason: UnavailableReason::NotSupported,
        },
        Observed::Stale { last_seen_at_ms: 7 },
        Observed::Error {
            code: ObservationError::Internal,
        },
    ];
    for estimate in missing {
        let mut report = auto_report();
        report.prelaunch.as_mut().unwrap().memory_estimate = estimate.clone();
        assert!(
            matches!(report.validate(), Err(ReportError::InvalidPrelaunch)),
            "auto with estimate {estimate:?}"
        );
    }
}

#[test]
fn validator_accepts_an_auto_limit_whatever_the_estimate() {
    // Bug: the rule's arithmetic frozen into the validator, so a report from a later rule (or a
    // limit above the estimate) stops parsing. The writing release owns the arithmetic.
    for estimate in [2 * ESTIMATE, LIMIT, LIMIT - 1, 1] {
        let mut report = auto_report();
        report.prelaunch.as_mut().unwrap().memory_estimate =
            Observed::Available { value: estimate };
        assert!(report.validate().is_ok(), "estimate {estimate}");
    }
}
