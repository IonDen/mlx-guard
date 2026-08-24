#![cfg(unix)]
//! Property suite proving the argv-projection gate (`RunIdentity::from_argv`) and full
//! `ReportV1` serialization never leak a planted marker outside the one legal channel
//! (`run.executable_basename`, derived from `argv[0]`'s final path component). Follows
//! `parser_fuzz.rs`'s established pattern: a hand-rolled xorshift `Generator(u64)` seeded from a
//! hex-ASCII constant, a `MAX_CASE_RUNTIME` wall guard asserted every iteration, and fixed case
//! volumes. This suite does not modify `report.rs`; it exercises only its public surface.
//!
//! The legal channel is *computed*, not flagged: every case derives
//! `expected = occurrences(marker, basename_projection(argv0))` from a test-local
//! `basename_projection` that independently mirrors `RunIdentity::from_argv`'s own basename
//! derivation (`Path::file_name()`, `<non-utf8>` substitution). Recomputing independently, rather
//! than reading back `identity.executable_basename`, is what lets this suite catch a mutated
//! `from_argv` that computes the wrong basename — reading the field back would be tautological.
//! The committed `corpus()` rows additionally carry a hand-pinned expected answer, checked
//! against their own construction before they run, so a shared mistake between the mirror and
//! the implementation can't silently cancel out.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::time::{Duration, Instant};

use mlx_guard_core::{
    AdvisoryFreshness, AdvisoryMetadata, AdvisoryMetricMetadata, AdvisoryMetrics, AdvisoryScope,
    AdvisorySource, CapturePolicy, CheckpointRecord, CheckpointStatus, ChildStatus, EscapeEvidence,
    MemoryPressureLevel, ObservationError, Observed, OnParentExit, ParentWatch, PolicyState,
    PrivacyDefaults, REPORT_SCHEMA_VERSION, ReportError, ReportMode, ReportV1, RetentionPolicy,
    RunIdentity, SampleWindow, SignalReason, SignalRecord, SignalResult, SignalTarget,
    TerminalKind, TerminalOutcome, TransitionRecord, UnavailableReason, UploadPolicy,
};

const MAX_CASE_RUNTIME: Duration = Duration::from_secs(10);
const PROPERTY_CASES: usize = 512;

/// The `run_id` contract pinned everywhere in this file except property 3, which fuzzes it.
const PINNED_RUN_ID: &str = "run_0123456789abcdef0123456789abcdef";

const SEED_PROPERTY_1: u64 = 0x6172_6776_7072_6f6a; // "argvproj"
const SEED_PROPERTY_2: u64 = 0x636f_7272_6861_7368; // "corrhash"
const SEED_PROPERTY_3: u64 = 0x7275_6e69_6461_6476; // "runidadv"
const SEED_PROPERTY_4: u64 = 0x6669_656c_6461_6476; // "fieldadv"
const SEED_PROPERTY_4_EARLY_GATE: u64 = 0x6669_656c_6467_6174; // "fieldgat"

/// Case volume for property 4's early-gate lane (see `adversarial_field_values_never_panic_validation`).
const EARLY_GATE_CASES: usize = 128;

/// Property 1's Ok-yield floor. Calibrated once against `corpus()` (10 rows, 7 Ok / 3 Err) plus
/// 512 randomized cases (`adversarial_argv0`/`adversarial_args`, biased ~80% toward a valid final
/// path component per the generator's class weights): the observed combined yield was 412/522 =
/// 78% Ok (measured with `SEED_PROPERTY_1` and this exact corpus/generator; recalibrate if either
/// changes). The floor is set well under that observed value so ordinary generator-seed drift
/// cannot make the property flaky, while still being far above zero so an accidental
/// always-Err regression (e.g. a broken validator) cannot pass vacuously.
const PROPERTY_1_OK_FLOOR_PERCENT: usize = 60;

struct Generator(u64);

impl Generator {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn index(&mut self, upper: usize) -> usize {
        usize::try_from(self.next()).unwrap_or(usize::MAX) % upper
    }

    fn bool(&mut self) -> bool {
        self.index(2) == 0
    }

    fn byte(&mut self) -> u8 {
        self.next().to_le_bytes()[0]
    }

    fn u32(&mut self) -> u32 {
        let bytes = self.next().to_le_bytes();
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }
}

/// Parse the committed golden fixture (the same one `parser_fuzz.rs:67` reads) into a real,
/// already-valid `ReportV1` that every property clones and mutates.
fn base_report() -> ReportV1 {
    let golden = include_str!("fixtures/report-v1.json");
    ReportV1::from_json(golden).expect("committed golden report-v1.json must parse and validate")
}

fn occurrences(marker: &str, haystack: &str) -> usize {
    haystack.matches(marker).count()
}

/// Test-local mirror of `RunIdentity::from_argv`'s basename derivation (report.rs:70-75):
/// `Path::file_name()`, then `<non-utf8>` substitution on a failed UTF-8 decode. Returns `None`
/// exactly when `from_argv` would fail at its own `.ok_or(InvalidIdentity)` step (no file-name
/// component at all), in which case the case is expected to be `Err` regardless of any marker.
fn basename_projection(argv0: &OsStr) -> Option<String> {
    Path::new(argv0).file_name().map(|name| {
        name.to_str()
            .map_or_else(|| "<non-utf8>".to_owned(), ToOwned::to_owned)
    })
}

fn always_valid_argv0() -> OsString {
    OsString::from("/tmp/RDCT_shared_dir/python")
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

fn lowercase_hex(generator: &mut Generator, length: usize) -> String {
    let bytes: Vec<u8> = (0..length)
        .map(|_| HEX_DIGITS[generator.index(16)])
        .collect();
    String::from_utf8(bytes).expect("the hex alphabet is always valid UTF-8")
}

fn adversarial_valid_correlation_hash(generator: &mut Generator) -> String {
    format!("sha256:{}", lowercase_hex(generator, 64))
}

const ASCII_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-";

fn ascii_padding(generator: &mut Generator, length: usize) -> Vec<u8> {
    (0..length)
        .map(|_| ASCII_ALPHABET[generator.index(ASCII_ALPHABET.len())])
        .collect()
}

// ---------------------------------------------------------------------------------------------
// CORPUS (committed, append-on-counterexample; each row is one adversarial class).
// ---------------------------------------------------------------------------------------------

struct CorpusCase {
    name: &'static str,
    argv0: OsString,
    args: Vec<OsString>,
    marker: String,
    /// Hand-pinned answer, independent of `basename_projection`/`from_argv`: `Some(n)` — this
    /// row is expected to project successfully with exactly `n` marker occurrences in the
    /// basename; `None` — this row is expected to be rejected. `assert_pinned_corpus_expectation`
    /// checks this against the row's own construction before `exercise_case` runs, so a bug that
    /// made the mirror and the real implementation silently agree on the wrong answer would still
    /// be caught here.
    expected: Option<usize>,
}

/// Ten committed, hand-picked adversarial rows, one per class named in the plan brief. Corpus
/// rows are exercised through the same `exercise_case` helper as the randomized cases: every
/// row's expected occurrence count is *computed* by `basename_projection`, never hardcoded, so a
/// counterexample discovered later can simply be appended as an eleventh row.
fn corpus() -> Vec<CorpusCase> {
    let benign_argv0 = || OsString::from("/tmp/RDCT_corpus_dir/python");
    let mut rows = Vec::new();

    // 1: marker embedded in an arg via a `--token=` style flag. Args are never captured by the
    //    schema (only `argument_count` is), so this must never survive regardless of argv0.
    let marker = "RDCT_CORPUS_1_c4n4ry".to_owned();
    rows.push(CorpusCase {
        name: "arg_token_flag",
        argv0: benign_argv0(),
        args: vec![OsString::from(format!("--token={marker}"))],
        marker,
        expected: Some(0),
    });

    // 2: marker followed by a trailing newline, still just an arg.
    let marker = "RDCT_CORPUS_2_c4n4ry".to_owned();
    rows.push(CorpusCase {
        name: "arg_trailing_newline",
        argv0: benign_argv0(),
        args: vec![OsString::from(format!("{marker}\n"))],
        marker,
        expected: Some(0),
    });

    // 3: marker wrapped in an ANSI color escape, still just an arg.
    let marker = "RDCT_CORPUS_3_c4n4ry".to_owned();
    rows.push(CorpusCase {
        name: "arg_ansi_escape",
        argv0: benign_argv0(),
        args: vec![OsString::from(format!("\x1b[31m{marker}m"))],
        marker,
        expected: Some(0),
    });

    // 4: the final path component is invalid UTF-8 (marker bytes plus a lone 0xFF byte), so
    //    `to_str()` fails and `from_argv` substitutes the fixed "<non-utf8>" string — the raw
    //    bytes, including the marker bytes inside them, are never copied (REPORTS.md's rule).
    //    expected == 0 by construction: "<non-utf8>" cannot contain the marker text.
    let marker = "RDCT_CORPUS_4_c4n4ry".to_owned();
    let mut raw = b"/tmp/RDCT_corpus_dir/".to_vec();
    raw.extend_from_slice(marker.as_bytes());
    raw.push(0xFF);
    rows.push(CorpusCase {
        name: "non_utf8_final_component",
        argv0: OsString::from_vec(raw),
        args: Vec::new(),
        marker,
        expected: Some(0),
    });

    // 5: marker lives in a *directory* component; the basename ("python") is innocent. Only the
    //    basename is captured, so expected == 0.
    let marker = "RDCT_CORPUS_5_c4n4ry".to_owned();
    rows.push(CorpusCase {
        name: "directory_marker_innocent_basename",
        argv0: OsString::from(format!("/tmp/{marker}dir/python")),
        args: Vec::new(),
        marker,
        expected: Some(0),
    });

    // 6: the marker IS the entire basename — the one legal channel. expected == 1.
    let marker = "RDCT_CORPUS_6_c4n4ry".to_owned();
    rows.push(CorpusCase {
        name: "marker_is_entire_basename",
        argv0: OsString::from(format!("/tmp/RDCT_corpus_dir/{marker}")),
        args: Vec::new(),
        marker,
        expected: Some(1),
    });

    // 7: the marker is a substring of an otherwise-ordinary basename (the I1 false-positive
    //    class: a real executable name like "python" could legitimately embed adversarial-
    //    looking text). Still the legal channel, still expected == 1.
    let marker = "RDCT_CORPUS_7_c4n4ry".to_owned();
    rows.push(CorpusCase {
        name: "marker_basename_substring",
        argv0: OsString::from(format!("/tmp/RDCT_corpus_dir/py{marker}thon")),
        args: Vec::new(),
        marker,
        expected: Some(1),
    });

    // 8/9: argv0 exactly "." / "..". `Path::file_name()` never yields a Normal component for a
    //    sole CurDir/ParentDir path element, so `from_argv` fails at its `.ok_or(InvalidIdentity)`
    //    step before validate()'s explicit "."/".." check is ever reached — still a typed Err.
    rows.push(CorpusCase {
        name: "argv0_dot",
        argv0: OsString::from("."),
        args: Vec::new(),
        marker: "RDCT_CORPUS_8_c4n4ry".to_owned(),
        expected: None,
    });
    rows.push(CorpusCase {
        name: "argv0_dotdot",
        argv0: OsString::from(".."),
        args: Vec::new(),
        marker: "RDCT_CORPUS_9_c4n4ry".to_owned(),
        expected: None,
    });

    // 10: a 256-byte basename (over the validator's 255-byte cap), embedding the marker for good
    //     measure — still a typed Err, so the marker never reaches serialization either way.
    let marker = "RDCT_CORPUS_10_c4n4ry".to_owned();
    let oversized_basename = format!("{marker}{}", "A".repeat(260));
    rows.push(CorpusCase {
        name: "oversized_basename",
        argv0: OsString::from(format!("/tmp/RDCT_corpus_dir/{oversized_basename}")),
        args: Vec::new(),
        marker,
        expected: None,
    });

    rows
}

/// Assert each corpus row's hand-pinned answer (see `CorpusCase::expected`'s doc) against the
/// row's own bytes, independent of `exercise_case`'s Ok/Err bookkeeping.
fn assert_pinned_corpus_expectation(case: &CorpusCase) {
    let projected = basename_projection(&case.argv0);
    if let Some(pinned_count) = case.expected {
        let name = projected.unwrap_or_else(|| {
            panic!(
                "case {}: pinned Ok({pinned_count}) but no basename projected at all",
                case.name
            )
        });
        assert_eq!(
            occurrences(&case.marker, &name),
            pinned_count,
            "case {}: pinned expected occurrence count does not match basename_projection",
            case.name
        );
    } else {
        let mut full_argv = vec![case.argv0.clone()];
        full_argv.extend(case.args.iter().cloned());
        assert!(
            RunIdentity::from_argv(PINNED_RUN_ID, &full_argv, None).is_err(),
            "case {}: pinned Err expectation did not hold",
            case.name
        );
    }
}

/// Shared exercise body for both corpus rows and randomized cases: build `argv`, project the
/// expected occurrence count independently of `from_argv`, call `from_argv`, and on `Ok` verify
/// both that the marker's occurrence count in the fully serialized report matches that
/// independent projection AND that every occurrence lives inside `run.executable_basename`. On
/// `Err`, count it as a typed rejection.
fn exercise_case(
    base: &ReportV1,
    case_name: &str,
    argv0: &OsStr,
    args: &[OsString],
    marker: &str,
    ok_count: &mut usize,
    err_count: &mut usize,
) {
    let mut full_argv = vec![argv0.to_os_string()];
    full_argv.extend_from_slice(args);
    let projected_basename = basename_projection(argv0);

    match RunIdentity::from_argv(PINNED_RUN_ID, &full_argv, None) {
        Ok(identity) => {
            *ok_count += 1;
            let expected = projected_basename.as_deref().map_or_else(
                || panic!("case {case_name}: Ok identity implies a projected basename existed"),
                |name| occurrences(marker, name),
            );

            let mut report = base.clone();
            report.run = identity;
            report.privacy.capture = CapturePolicy::RedactedMetadataOnly;
            let serialized = report.to_json_pretty().unwrap_or_else(|error| {
                panic!("case {case_name}: base report stayed valid: {error}")
            });

            let total = occurrences(marker, &serialized);
            let parsed: serde_json::Value =
                serde_json::from_str(&serialized).expect("just-serialized report is valid JSON");
            let basename_value = parsed["run"]["executable_basename"]
                .as_str()
                .expect("executable_basename is always a JSON string");
            let within_basename = occurrences(marker, basename_value);

            assert_eq!(
                total, expected,
                "case {case_name}: marker occurrence count drifted from the projected basename"
            );
            assert_eq!(
                within_basename, expected,
                "case {case_name}: every marker occurrence must live inside executable_basename"
            );
        }
        Err(error) => {
            *err_count += 1;
            assert!(
                matches!(error, ReportError::InvalidIdentity),
                "case {case_name}: unexpected error variant {error:?}"
            );
        }
    }
}

fn basename_bytes_for_class(generator: &mut Generator, marker: &str) -> Vec<u8> {
    match generator.index(10) {
        0 | 1 => marker.as_bytes().to_vec(),
        2 | 3 => {
            let prefix_length = generator.index(20);
            let mut bytes = ascii_padding(generator, prefix_length);
            bytes.extend_from_slice(marker.as_bytes());
            let suffix_length = generator.index(20);
            bytes.extend_from_slice(&ascii_padding(generator, suffix_length));
            bytes
        }
        4..=6 => {
            let length = 1 + generator.index(40);
            ascii_padding(generator, length)
        }
        7 => {
            let mut bytes = marker.as_bytes().to_vec();
            bytes.push(0xFF);
            bytes
        }
        _ => {
            let length = 256 + generator.index(50);
            ascii_padding(generator, length)
        }
    }
}

fn salted_directory_component(generator: &mut Generator, marker: &str) -> Vec<u8> {
    let length = 1 + generator.index(12);
    let mut bytes = ascii_padding(generator, length);
    if generator.index(3) == 0 {
        let position = generator.index(bytes.len() + 1);
        let mut salted = bytes[..position].to_vec();
        salted.extend_from_slice(marker.as_bytes());
        salted.extend_from_slice(&bytes[position..]);
        bytes = salted;
    }
    bytes
}

/// Adversarial argv0: a directory component randomly salted with the marker (never captured, so
/// never expected to leak) joined to a final component chosen from `basename_bytes_for_class`
/// (biased toward valid so the yield floor stays honest — see `PROPERTY_1_OK_FLOOR_PERCENT`).
fn adversarial_argv0(generator: &mut Generator, marker: &str) -> OsString {
    let directory = salted_directory_component(generator, marker);
    let basename = basename_bytes_for_class(generator, marker);
    let mut raw = b"/tmp/".to_vec();
    raw.extend_from_slice(&directory);
    raw.push(b'/');
    raw.extend_from_slice(&basename);
    OsString::from_vec(raw)
}

fn adversarial_arg(generator: &mut Generator, marker: &str) -> OsString {
    match generator.index(5) {
        0 => OsString::from(format!("--token={marker}")),
        1 => OsString::from(format!("{marker}\n")),
        2 => OsString::from(format!("\x1b[31m{marker}m")),
        3 => {
            let mut bytes = marker.as_bytes().to_vec();
            bytes.push(0xFF);
            OsString::from_vec(bytes)
        }
        _ => {
            let length = generator.index(32);
            OsString::from_vec(ascii_padding(generator, length))
        }
    }
}

fn adversarial_args(generator: &mut Generator, marker: &str) -> Vec<OsString> {
    let count = generator.index(4);
    (0..count)
        .map(|_| adversarial_arg(generator, marker))
        .collect()
}

/// Adversarial argv0 + N adversarial args never survive projection into the serialized report,
/// except through the one legal channel (`executable_basename`, derived from argv0's final path
/// component), and only in the exact computed amount.
#[test]
fn argv_markers_never_survive_projection() {
    let started = Instant::now();
    let base = base_report();
    let mut ok_count = 0usize;
    let mut err_count = 0usize;

    for case in corpus() {
        assert!(started.elapsed() < MAX_CASE_RUNTIME);
        assert_pinned_corpus_expectation(&case);
        exercise_case(
            &base,
            case.name,
            &case.argv0,
            &case.args,
            &case.marker,
            &mut ok_count,
            &mut err_count,
        );
    }

    let mut generator = Generator(SEED_PROPERTY_1);
    for case in 0..PROPERTY_CASES {
        assert!(started.elapsed() < MAX_CASE_RUNTIME);
        let marker = format!("RDCT_P1_{case}_c4n4ry");
        let argv0 = adversarial_argv0(&mut generator, &marker);
        let args = adversarial_args(&mut generator, &marker);
        let case_label = format!("{case} of {PROPERTY_CASES}");
        exercise_case(
            &base,
            &case_label,
            &argv0,
            &args,
            &marker,
            &mut ok_count,
            &mut err_count,
        );
    }

    assert!(started.elapsed() < MAX_CASE_RUNTIME);
    let total = ok_count + err_count;
    assert!(
        ok_count * 100 >= total * PROPERTY_1_OK_FLOOR_PERCENT,
        "Ok yield {ok_count}/{total} fell under the {PROPERTY_1_OK_FLOOR_PERCENT}% calibrated floor"
    );
}

/// `correlation_hash` is caller-supplied and only ever round-trips verbatim; it is never derived
/// from argv or anything else. `None` produces no `correlation_hash` key at all. Every case here
/// unwraps: an always-valid basename and the pinned `run_id` make an `Err` a genuine test failure,
/// not a tolerated outcome (C3 — no vacuous passes via Err-tolerance).
#[test]
fn correlation_hash_is_caller_supplied_never_derived() {
    let started = Instant::now();
    let base = base_report();
    let mut generator = Generator(SEED_PROPERTY_2);
    let argv = vec![always_valid_argv0()];

    for _ in 0..PROPERTY_CASES {
        assert!(started.elapsed() < MAX_CASE_RUNTIME);
        if generator.bool() {
            let identity = RunIdentity::from_argv(PINNED_RUN_ID, &argv, None)
                .expect("always-valid basename and pinned run_id must project");
            let mut report = base.clone();
            report.run = identity;
            report.privacy.capture = CapturePolicy::RedactedMetadataOnly;
            let serialized = report
                .to_json_pretty()
                .expect("a None-hash identity paired with RedactedMetadataOnly must validate");
            assert!(!serialized.contains("\"correlation_hash\""));
        } else {
            let hash = adversarial_valid_correlation_hash(&mut generator);
            let identity = RunIdentity::from_argv(PINNED_RUN_ID, &argv, Some(&hash))
                .expect("always-valid basename, pinned run_id, and a valid hash must project");
            let mut report = base.clone();
            report.run = identity;
            let serialized = report
                .to_json_pretty()
                .expect("golden's RedactedMetadataWithCorrelationHash matches a Some(hash) run");
            let parsed = ReportV1::from_json(&serialized).expect("just-serialized report parses");
            assert_eq!(parsed.run.correlation_hash.as_deref(), Some(hash.as_str()));
        }
    }

    assert!(started.elapsed() < MAX_CASE_RUNTIME);
}

/// Wrong prefix / wrong length / wrong alphabet, each guaranteed invalid by construction (never
/// left to chance), cycled deterministically so every class gets roughly a third of the volume.
fn adversarial_invalid_run_id(generator: &mut Generator, case: usize) -> String {
    const BAD_PREFIXES: &[&str] = &["id_", "RUN_", "sess_", "proc_", "walk_", ""];
    const INVALID_CHARS: &[u8] = b"GXZ!_ \t";

    match case % 3 {
        0 => {
            let prefix = BAD_PREFIXES[generator.index(BAD_PREFIXES.len())];
            format!("{prefix}{}", lowercase_hex(generator, 32))
        }
        1 => {
            let mut length = generator.index(64);
            if length == 32 {
                length += 1;
            }
            format!("run_{}", lowercase_hex(generator, length))
        }
        _ => {
            let mut hex = lowercase_hex(generator, 32).into_bytes();
            let position = generator.index(hex.len());
            hex[position] = INVALID_CHARS[generator.index(INVALID_CHARS.len())];
            format!(
                "run_{}",
                String::from_utf8(hex).expect("mutated hex stays ASCII")
            )
        }
    }
}

/// Every adversarial `run_id` (wrong prefix, wrong length, wrong alphabet) is rejected — a
/// positive `Err` assertion with no tolerance, unlike properties 1 and 4.
#[test]
fn adversarial_run_ids_are_rejected() {
    let started = Instant::now();
    let mut generator = Generator(SEED_PROPERTY_3);
    let argv = vec![always_valid_argv0()];

    for case in 0..PROPERTY_CASES {
        assert!(started.elapsed() < MAX_CASE_RUNTIME);
        let run_id = adversarial_invalid_run_id(&mut generator, case);
        let result = RunIdentity::from_argv(&run_id, &argv, None);
        assert!(
            matches!(result, Err(ReportError::InvalidIdentity)),
            "adversarial run_id {run_id:?} unexpectedly produced {result:?}"
        );
    }

    assert!(started.elapsed() < MAX_CASE_RUNTIME);
}

// ---------------------------------------------------------------------------------------------
// Property 4 — adversarial TYPED field values through `validate()`, never a panic.
//
// `parser_fuzz.rs`'s `report_parser_handles_bounded_mutated_and_jsonish_inputs_without_panicking`
// byte-flips raw JSON text; the overwhelming majority of those mutations produce malformed JSON
// that `serde_json` rejects before `validate()` ever runs, so it rarely reaches deep, precise
// cross-field invariants (footprint ordering, sample/transition/signal ordering, checkpoint
// status vs. timestamp agreement, escape count vs. detection agreement, and so on). This property
// instead constructs a real, strongly-typed `ReportV1` directly (bypassing JSON entirely) and
// mutates individual fields to adversarial-but-well-typed values, so `validate()`'s own
// cross-field logic is what gets exercised, not the JSON parser's error path.
// ---------------------------------------------------------------------------------------------

const ADVERSARIAL_CHARS: &[char] = &[
    'a', 'Z', '0', '9', '_', '-', '.', '/', '\\', '\n', '\t', '\0', '\x1b', ' ', '"', '\'', 'é',
    '💥', '\u{200b}',
];

fn adversarial_string(generator: &mut Generator, length: usize) -> String {
    (0..length)
        .map(|_| ADVERSARIAL_CHARS[generator.index(ADVERSARIAL_CHARS.len())])
        .collect()
}

fn adversarial_observed<T>(
    generator: &mut Generator,
    value: impl FnOnce(&mut Generator) -> T,
) -> Observed<T> {
    match generator.index(5) {
        0 => Observed::Available {
            value: value(generator),
        },
        1 => Observed::Unknown,
        2 => Observed::Unavailable {
            reason: adversarial_unavailable_reason(generator),
        },
        3 => Observed::Stale {
            last_seen_at_ms: generator.next() % 5000,
        },
        _ => Observed::Error {
            code: adversarial_observation_error(generator),
        },
    }
}

fn adversarial_unavailable_reason(generator: &mut Generator) -> UnavailableReason {
    const REASONS: &[UnavailableReason] = &[
        UnavailableReason::NotSupported,
        UnavailableReason::NotNegotiated,
        UnavailableReason::NotApplicable,
        UnavailableReason::NonUtf8,
    ];
    REASONS[generator.index(REASONS.len())]
}

fn adversarial_observation_error(generator: &mut Generator) -> ObservationError {
    const ERRORS: &[ObservationError] = &[
        ObservationError::PermissionDenied,
        ObservationError::ProcessMissing,
        ObservationError::MalformedKernelData,
        ObservationError::ClockAnomaly,
        ObservationError::Internal,
    ];
    ERRORS[generator.index(ERRORS.len())]
}

fn adversarial_observed_u64(generator: &mut Generator) -> Observed<u64> {
    adversarial_observed(generator, |g| g.next() % 1_000_000)
}

fn adversarial_observed_i64(generator: &mut Generator) -> Observed<i64> {
    adversarial_observed(generator, |g| {
        i64::try_from(g.index(2000)).unwrap_or(0) - 1000
    })
}

fn adversarial_pressure_level(generator: &mut Generator) -> MemoryPressureLevel {
    const LEVELS: &[MemoryPressureLevel] = &[
        MemoryPressureLevel::Normal,
        MemoryPressureLevel::Warning,
        MemoryPressureLevel::Critical,
    ];
    LEVELS[generator.index(LEVELS.len())]
}

fn adversarial_metadata_entry(generator: &mut Generator) -> AdvisoryMetricMetadata {
    const SCOPES: &[AdvisoryScope] = &[AdvisoryScope::System, AdvisoryScope::OwnedProcessGroup];
    const SOURCES: &[AdvisorySource] = &[
        AdvisorySource::DispatchMemoryPressure,
        AdvisorySource::SysctlVmSwapusage,
        AdvisorySource::HostStatistics64,
        AdvisorySource::DerivedFootprintSamples,
    ];
    const FRESHNESS: &[AdvisoryFreshness] = &[
        AdvisoryFreshness::Fresh,
        AdvisoryFreshness::InitialUnknown,
        AdvisoryFreshness::Stale,
        AdvisoryFreshness::Unavailable,
    ];
    AdvisoryMetricMetadata {
        scope: SCOPES[generator.index(SCOPES.len())],
        source: SOURCES[generator.index(SOURCES.len())],
        captured_at_ms: generator.next() % 10_000,
        freshness: FRESHNESS[generator.index(FRESHNESS.len())],
    }
}

fn adversarial_advisory_metrics(generator: &mut Generator) -> AdvisoryMetrics {
    let pressure_events = adversarial_observed_u64(generator);
    let swap_bytes = adversarial_observed_u64(generator);
    let compressor_bytes = adversarial_observed_u64(generator);
    let wired_bytes = adversarial_observed_u64(generator);
    let growth_bytes_per_second = adversarial_observed_i64(generator);
    let has_metadata = generator.bool();
    let pressure_level =
        has_metadata.then(|| adversarial_observed(generator, adversarial_pressure_level));
    let metadata = has_metadata.then(|| AdvisoryMetadata {
        pressure_events: adversarial_metadata_entry(generator),
        pressure_level: adversarial_metadata_entry(generator),
        swap_bytes: adversarial_metadata_entry(generator),
        compressor_bytes: adversarial_metadata_entry(generator),
        wired_bytes: adversarial_metadata_entry(generator),
        growth_bytes_per_second: adversarial_metadata_entry(generator),
    });
    AdvisoryMetrics {
        pressure_events,
        swap_bytes,
        compressor_bytes,
        wired_bytes,
        growth_bytes_per_second,
        pressure_level,
        metadata,
    }
}

fn adversarial_sample_window(generator: &mut Generator) -> SampleWindow {
    SampleWindow {
        captured_at_ms: generator.next() % 10_000,
        processed_at_ms: generator.next() % 10_000,
        window_ms: generator.next() % 1000,
        aggregate_footprint_bytes: adversarial_observed_u64(generator),
        advisory: adversarial_advisory_metrics(generator),
    }
}

fn adversarial_policy_state(generator: &mut Generator) -> PolicyState {
    const STATES: &[PolicyState] = &[
        PolicyState::Observe,
        PolicyState::Normal,
        PolicyState::Warning,
        PolicyState::CheckpointRequested,
        PolicyState::Terminating,
        PolicyState::Emergency,
        PolicyState::Exited,
        PolicyState::SupervisorError,
    ];
    STATES[generator.index(STATES.len())]
}

fn adversarial_transition(generator: &mut Generator) -> TransitionRecord {
    TransitionRecord {
        at_ms: generator.next() % 10_000,
        from: adversarial_policy_state(generator),
        to: adversarial_policy_state(generator),
        aggregate_footprint_bytes: generator.bool().then(|| generator.next() % 1_000_000),
    }
}

fn adversarial_signal(generator: &mut Generator) -> SignalRecord {
    const TARGETS: &[SignalTarget] = &[
        SignalTarget::OwnedProcessGroup,
        SignalTarget::CooperativeEndpoint,
    ];
    const RESULTS: &[SignalResult] = &[
        SignalResult::Delivered,
        SignalResult::ProcessMissing,
        SignalResult::PermissionDenied,
        SignalResult::Failed,
    ];
    const REASONS: &[SignalReason] = &[
        SignalReason::Footprint,
        SignalReason::WallTime,
        SignalReason::ExternalSignal,
        SignalReason::RootExitCleanup,
        SignalReason::ParentExit,
        SignalReason::ObservationFailure,
        SignalReason::SupervisorFault,
    ];
    SignalRecord {
        at_ms: generator.next() % 10_000,
        signal: generator.byte(),
        target: TARGETS[generator.index(TARGETS.len())],
        result: RESULTS[generator.index(RESULTS.len())],
        reason: generator
            .bool()
            .then(|| REASONS[generator.index(REASONS.len())]),
    }
}

fn adversarial_terminal_kind(generator: &mut Generator) -> TerminalKind {
    match generator.index(8) {
        0 => TerminalKind::ChildExited {
            code: generator.byte(),
        },
        1 => TerminalKind::ChildSignaled {
            signal: generator.byte(),
        },
        2 => TerminalKind::LaunchNotFound,
        3 => TerminalKind::LaunchNotExecutable,
        4 => TerminalKind::InvalidConfiguration,
        5 => TerminalKind::PolicyIntervention,
        6 => TerminalKind::SupervisorFailure,
        _ => TerminalKind::PartialArtifactFailure,
    }
}

fn adversarial_child_status(generator: &mut Generator) -> ChildStatus {
    if generator.bool() {
        ChildStatus::Exited {
            code: generator.byte(),
        }
    } else {
        ChildStatus::Signaled {
            signal: generator.byte(),
        }
    }
}

type FieldMutation = fn(&mut Generator, &mut ReportV1);

/// Guaranteed-invalid by construction (never left to chance): bumps past `REPORT_SCHEMA_VERSION`
/// on the rare collision so the early-gate lane's positive `Err` assertion never flakes.
fn mutate_schema_version(generator: &mut Generator, report: &mut ReportV1) {
    let mut version = generator.u32();
    if version == REPORT_SCHEMA_VERSION {
        version = version.wrapping_add(1);
    }
    report.schema_version = version;
}

/// Guaranteed-invalid by construction: a trailing space is never in `valid_package_version`'s
/// allowed charset, so this always fails regardless of the random prefix.
fn mutate_package_version(generator: &mut Generator, report: &mut ReportV1) {
    let length = 1 + generator.index(30);
    let mut value = adversarial_string(generator, length);
    value.push(' ');
    report.package_version = value;
}

fn mutate_run_identity(generator: &mut Generator, report: &mut ReportV1) {
    match generator.index(4) {
        0 => {
            let class = generator.index(3);
            report.run.run_id = adversarial_invalid_run_id(generator, class);
        }
        1 => {
            let length = generator.index(300);
            report.run.executable_basename = adversarial_string(generator, length);
        }
        2 => report.run.argument_count = generator.u32(),
        _ => {
            report.run.correlation_hash = generator.bool().then(|| {
                let length = generator.index(80);
                adversarial_string(generator, length)
            });
        }
    }
}

fn mutate_configuration_mode(generator: &mut Generator, report: &mut ReportV1) {
    report.configuration.mode = if generator.bool() {
        ReportMode::Observe
    } else {
        ReportMode::Enforce
    };
}

fn mutate_configuration_thresholds(generator: &mut Generator, report: &mut ReportV1) {
    report.configuration.recovery_footprint_bytes =
        generator.bool().then(|| generator.next() % 1_000_000);
    report.configuration.warning_footprint_bytes =
        generator.bool().then(|| generator.next() % 1_000_000);
    report.configuration.max_footprint_bytes =
        generator.bool().then(|| generator.next() % 1_000_000);
    report.configuration.emergency_footprint_bytes =
        generator.bool().then(|| generator.next() % 1_000_000);
}

fn mutate_configuration_timing(generator: &mut Generator, report: &mut ReportV1) {
    report.configuration.required_breach_samples = generator.u32();
    report.configuration.max_missing_samples = generator.u32();
    report.configuration.sample_interval_ms = generator.next() % 5000;
    report.configuration.max_sample_age_ms = generator.next() % 5000;
    report.configuration.max_sample_window_ms = generator.next() % 5000;
    report.configuration.term_grace_ms = generator.next() % 5000;
    report.configuration.wall_time_ms = generator.bool().then(|| generator.next() % 5000);
    report.configuration.checkpoint_timeout_ms = generator.bool().then(|| generator.next() % 5000);
}

fn mutate_parent_exit_pair(generator: &mut Generator, report: &mut ReportV1) {
    const OPTIONS: &[Option<OnParentExit>] = &[
        None,
        Some(OnParentExit::Terminate),
        Some(OnParentExit::Detach),
    ];
    const WATCHES: &[Option<ParentWatch>] = &[
        None,
        Some(ParentWatch::Active),
        Some(ParentWatch::ParentIsLaunchd),
        Some(ParentWatch::HangupIgnored),
        Some(ParentWatch::Detach),
        Some(ParentWatch::ParentUnobservable),
    ];
    report.configuration.on_parent_exit = OPTIONS[generator.index(OPTIONS.len())];
    report.configuration.parent_watch = WATCHES[generator.index(WATCHES.len())];
}

fn mutate_samples(generator: &mut Generator, report: &mut ReportV1) {
    let count = generator.index(4);
    report.samples = (0..count)
        .map(|_| adversarial_sample_window(generator))
        .collect();
}

fn mutate_transitions(generator: &mut Generator, report: &mut ReportV1) {
    let count = generator.index(4);
    report.transitions = (0..count)
        .map(|_| adversarial_transition(generator))
        .collect();
}

fn mutate_signals(generator: &mut Generator, report: &mut ReportV1) {
    let count = generator.index(4);
    report.signals = (0..count).map(|_| adversarial_signal(generator)).collect();
}

fn mutate_checkpoint(generator: &mut Generator, report: &mut ReportV1) {
    const STATUSES: &[CheckpointStatus] = &[
        CheckpointStatus::NotNegotiated,
        CheckpointStatus::RequestedUnverified,
        CheckpointStatus::AcknowledgedUnverifiedDurability,
        CheckpointStatus::TimedOut,
        CheckpointStatus::Cancelled,
    ];
    report.checkpoint = CheckpointRecord {
        status: STATUSES[generator.index(STATUSES.len())],
        at_ms: generator.bool().then(|| generator.next() % 10_000),
    };
}

fn mutate_escape(generator: &mut Generator, report: &mut ReportV1) {
    report.escape = EscapeEvidence {
        detected: adversarial_observed(generator, Generator::bool),
        escaped_count: generator.bool().then(|| 1 + generator.next() % 100),
    };
}

fn mutate_outcome(generator: &mut Generator, report: &mut ReportV1) {
    report.outcome = TerminalOutcome {
        at_ms: generator.next() % 10_000,
        kind: adversarial_terminal_kind(generator),
        final_footprint_bytes: adversarial_observed_u64(generator),
        child_status: generator
            .bool()
            .then(|| adversarial_child_status(generator)),
        owned_group_survivors: generator.bool().then(|| generator.bool()),
        parent_exited_at_ms: generator.bool().then(|| generator.next() % 10_000),
    };
}

/// `UploadPolicy` and `RetentionPolicy` are single-variant enums frozen for v0.1. `unsafe_code` is
/// denied workspace-wide, so no safely-typed `ReportV1` can ever hold a value other than
/// `Disabled`/`UserManaged` for these two fields — `validate_privacy`'s corresponding checks
/// (`upload != Disabled`, `retention != UserManaged`) are structurally unreachable via any real
/// construction path, not just this test, so there is nothing adversarial to mutate them to. The
/// other three `validate_privacy` invariants (`redacted_before_persistence`, `file_mode`, and
/// `capture` agreeing with `run.correlation_hash`'s presence) ARE exercised below, and
/// `adversarial_field_values_never_panic_validation` asserts `invalid_privacy > 0` as evidence.
fn mutate_privacy(generator: &mut Generator, report: &mut ReportV1) {
    report.privacy = PrivacyDefaults {
        redacted_before_persistence: generator.bool(),
        file_mode: if generator.bool() {
            "0600".to_owned()
        } else {
            let length = generator.index(8);
            adversarial_string(generator, length)
        },
        upload: UploadPolicy::Disabled,
        retention: RetentionPolicy::UserManaged,
        capture: if generator.bool() {
            CapturePolicy::RedactedMetadataOnly
        } else {
            CapturePolicy::RedactedMetadataWithCorrelationHash
        },
    };
}

/// The 12 "deep" mutators — deliberately excludes `mutate_schema_version`/`mutate_package_version`
/// (`validate()`'s first two gates; exercised in their own early-gate lane below) so a mutation from
/// this pool can't be masked behind an early rejection before reaching the cross-field invariants
/// in `validate_configuration`/`validate_privacy`/`validate_events`.
const FIELD_MUTATIONS: &[FieldMutation] = &[
    mutate_run_identity,
    mutate_configuration_mode,
    mutate_configuration_thresholds,
    mutate_configuration_timing,
    mutate_parent_exit_pair,
    mutate_samples,
    mutate_transitions,
    mutate_signals,
    mutate_checkpoint,
    mutate_escape,
    mutate_outcome,
    mutate_privacy,
];

/// Per-`ReportError`-variant rejection counts for the main mutation pool, so a coverage claim
/// ("this reaches the events/configuration layer") is backed by which variant actually fired, not
/// just that *some* `Err(_)` occurred.
#[derive(Debug, Default)]
struct ErrorTally {
    invalid_identity: usize,
    unsupported_schema: usize,
    invalid_package_version: usize,
    invalid_configuration: usize,
    invalid_privacy: usize,
    invalid_event_order: usize,
    invalid_advisory_metrics: usize,
    json: usize,
}

impl ErrorTally {
    fn record(&mut self, error: &ReportError) {
        match error {
            ReportError::InvalidIdentity => self.invalid_identity += 1,
            ReportError::UnsupportedSchema => self.unsupported_schema += 1,
            ReportError::InvalidPackageVersion => self.invalid_package_version += 1,
            ReportError::InvalidConfiguration => self.invalid_configuration += 1,
            ReportError::InvalidPrivacy => self.invalid_privacy += 1,
            ReportError::InvalidEventOrder => self.invalid_event_order += 1,
            ReportError::InvalidAdvisoryMetrics => self.invalid_advisory_metrics += 1,
            ReportError::Json(_) => self.json += 1,
        }
    }
}

/// Structurally valid reports carrying adversarial TYPED field values through `validate()` must
/// return `Ok` or a typed `Err` — never panic. Two lanes: an EARLY-GATE lane exercises
/// `schema_version`/`package_version` (`validate()`'s first two gates) in isolation with a
/// positive typed-`Err` assertion each, since mixing them into the main pool would mask every
/// deeper mutation behind an early rejection. The MAIN pool (the 12 deep mutators) is tallied by
/// `ReportError` variant, so this property proves — rather than just claims — that it reaches the
/// events/configuration cross-field invariants `parser_fuzz.rs`'s byte-mutation fuzz rarely does.
#[test]
fn adversarial_field_values_never_panic_validation() {
    let started = Instant::now();
    let base = base_report();

    let mut early_gate_generator = Generator(SEED_PROPERTY_4_EARLY_GATE);
    for case in 0..EARLY_GATE_CASES {
        assert!(started.elapsed() < MAX_CASE_RUNTIME);
        let mut candidate = base.clone();
        if case % 2 == 0 {
            mutate_schema_version(&mut early_gate_generator, &mut candidate);
            assert!(
                matches!(candidate.validate(), Err(ReportError::UnsupportedSchema)),
                "early-gate case {case} of {EARLY_GATE_CASES}: mutated schema_version must be \
                 rejected as UnsupportedSchema"
            );
        } else {
            mutate_package_version(&mut early_gate_generator, &mut candidate);
            assert!(
                matches!(
                    candidate.validate(),
                    Err(ReportError::InvalidPackageVersion)
                ),
                "early-gate case {case} of {EARLY_GATE_CASES}: mutated package_version must be \
                 rejected as InvalidPackageVersion"
            );
        }
    }
    assert!(started.elapsed() < MAX_CASE_RUNTIME);

    let mut generator = Generator(SEED_PROPERTY_4);
    let mut ok_count = 0usize;
    let mut tally = ErrorTally::default();
    for _ in 0..PROPERTY_CASES {
        assert!(started.elapsed() < MAX_CASE_RUNTIME);
        let mut candidate = base.clone();
        let mutations = 1 + generator.index(FIELD_MUTATIONS.len());
        for _ in 0..mutations {
            let mutation = FIELD_MUTATIONS[generator.index(FIELD_MUTATIONS.len())];
            mutation(&mut generator, &mut candidate);
        }
        match candidate.validate() {
            Ok(()) => ok_count += 1,
            Err(error) => tally.record(&error),
        }
    }

    assert!(started.elapsed() < MAX_CASE_RUNTIME);
    assert!(
        ok_count > 0,
        "no mutated case ever validated (weak mutation coverage)"
    );
    assert_eq!(
        tally.unsupported_schema + tally.invalid_package_version,
        0,
        "the main mutation pool must never touch the early-gate fields: {tally:?}"
    );
    let events_or_configuration =
        tally.invalid_configuration + tally.invalid_event_order + tally.invalid_advisory_metrics;
    assert!(
        events_or_configuration > 0,
        "no rejection reached the events/configuration cross-field invariants — this property's \
         claimed delta over parser_fuzz.rs's byte-mutation fuzz: {tally:?}"
    );
    assert!(
        tally.invalid_privacy > 0,
        "no rejection reached validate_privacy's achievable invariants: {tally:?}"
    );
}
