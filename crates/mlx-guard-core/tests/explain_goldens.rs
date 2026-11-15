//! Golden tests for the two plain outcome lines.
//!
//! Every case in `fixtures/explain/cases.json` names a report (repository-relative) and the
//! exact unprefixed text the explainer must render for it. The URL in each text carries the
//! literal `{version}` placeholder, which is replaced by the report's own `package_version`, so
//! the 0.1.0 fixtures link `v0.1.0` and the 0.2.0 tutorial reports link `v0.2.0`. The Python
//! mirror reads the same corpus and `anchors.txt`, so a drift between the two languages turns one
//! of them red.

use std::fs;
use std::path::{Path, PathBuf};

use mlx_guard_core::explain::{
    ANCHORS, EarlyCause, PlainOutcome, StorageClass, StoragePhase, explain_early, explain_report,
};
use mlx_guard_core::{
    AutoLimitRecord, AutoLimitRule, ChildStatus, LimitSource, MemoryCategories, Observed,
    PrelaunchRecord, ReportV1, TerminalKind, VERSION,
};
use serde::Deserialize;

const EXPLAIN_FIXTURES: &str = "crates/mlx-guard-core/tests/fixtures/explain";

/// The three reports of runs stopped at an automatic limit (breach value with TERM, no breach
/// value, emergency KILL), each a copy of its explicit sibling plus the two automatic fields.
const AUTO_FIXTURES: [&str; 3] = [
    "report-75-footprint-auto.json",
    "report-75-footprint-auto-no-breach.json",
    "report-75-emergency-auto.json",
];

/// A typed-size report of a footprint stop with a 6.00 GiB limit and no checkpoint.
const FOOTPRINT_REPORT: &str = "evidence/v0.2.0/tutorial/reports/run-limit.json";
/// An emergency KILL against the same 6.00 GiB limit.
const EMERGENCY_REPORT: &str =
    "crates/mlx-guard-core/tests/fixtures/explain/report-75-emergency.json";
/// The illustrative automatic limit: 6.00 GiB, three quarters of an 8.00 GiB estimate.
const AUTO_LIMIT: u64 = 6_442_450_944;
const AUTO_ESTIMATE: u64 = 8_589_934_592;

/// The prelaunch record a 0.5.0 or later run writes: an 8.00 GiB estimate, no warning, and the
/// automatic-limit record only when one is given.
fn prelaunch_8_gib(auto_limit: Option<AutoLimitRecord>) -> PrelaunchRecord {
    PrelaunchRecord {
        memory_estimate: Observed::Available {
            value: AUTO_ESTIMATE,
        },
        memory_categories: Observed::Available {
            value: MemoryCategories {
                free_bytes: 1_073_741_824,
                active_bytes: 3_221_225_472,
                inactive_bytes: 3_221_225_472,
                wired_bytes: 2_147_483_648,
                compressor_bytes: 1_073_741_824,
            },
        },
        warnings: Vec::new(),
        auto_limit,
    }
}

/// `relative` as a typed-size run of this release writes it: a prelaunch record, and the source
/// field either absent (0.5.0) or `explicit`.
fn explicit_with_prelaunch(relative: &str, source: Option<LimitSource>) -> ReportV1 {
    let mut report = load_report(relative);
    report.prelaunch = Some(prelaunch_8_gib(None));
    report.configuration.max_footprint_source = source;
    report
        .validate()
        .unwrap_or_else(|error| panic!("{relative} with a prelaunch record: {error}"));
    report
}

/// `relative` as an automatic run records it: source `auto`, and the rule's record with the
/// report's own limit.
fn as_automatic(relative: &str, rule: AutoLimitRule) -> ReportV1 {
    let mut report = load_report(relative);
    assert_eq!(
        report.configuration.max_footprint_bytes,
        Some(AUTO_LIMIT),
        "{relative} must hold the illustrative 6.00 GiB limit"
    );
    report.prelaunch = Some(prelaunch_8_gib(Some(AutoLimitRecord {
        rule,
        limit_bytes: AUTO_LIMIT,
    })));
    report.configuration.max_footprint_source = Some(LimitSource::Auto);
    report
        .validate()
        .unwrap_or_else(|error| panic!("{relative} as an automatic run: {error}"));
    report
}

fn rendered(report: &ReportV1) -> String {
    explain_report(report)
        .expect("a policy intervention has plain lines")
        .text()
}

/// One `cases.json` row. A row may also carry a `note` for readers, which the tests ignore.
#[derive(Debug, Deserialize)]
struct Case {
    row: String,
    report: String,
    text: String,
}

/// One `early.json` entry: a report-less cause, spelled as words, and the exact text it renders.
#[derive(Debug, Deserialize)]
struct EarlyCase {
    cause: String,
    text: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_repo_file(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn load_report(relative: &str) -> ReportV1 {
    ReportV1::from_json(&read_repo_file(relative))
        .unwrap_or_else(|error| panic!("{relative} is not a valid schema-v1 report: {error}"))
}

fn load_explain_fixture(name: &str) -> ReportV1 {
    load_report(&format!("{EXPLAIN_FIXTURES}/{name}"))
}

fn cases() -> Vec<Case> {
    serde_json::from_str(&read_repo_file(&format!("{EXPLAIN_FIXTURES}/cases.json")))
        .expect("cases.json must be a JSON array of {row, report, text}")
}

fn find_case(row_id: &str) -> Case {
    let mut matching: Vec<Case> = cases()
        .into_iter()
        .filter(|case| case.row == row_id)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "cases.json must hold exactly one case for row {row_id}"
    );
    matching.remove(0)
}

fn expected_text(case: &Case, report: &ReportV1) -> String {
    case.text.replace("{version}", &report.package_version)
}

fn prefixed_lines(text: &str) -> String {
    let mut lines = String::new();
    for line in text.split('\n') {
        lines.push_str("mlx-guard: ");
        lines.push_str(line);
        lines.push('\n');
    }
    lines
}

/// Compare every output form of one rendered outcome with its golden text.
fn check_outcome(label: &str, outcome: &PlainOutcome, expected: &str) -> Result<(), String> {
    if outcome.text() != expected {
        return Err(format!(
            "{label}:\n  rendered {:?}\n  golden   {expected:?}",
            outcome.text()
        ));
    }
    if outcome.stderr_lines() != prefixed_lines(expected) {
        return Err(format!(
            "{label}: the stderr form must be the same two lines, prefixed and newline-ended"
        ));
    }
    let Some((_, anchor)) = expected.rsplit_once('#') else {
        return Err(format!("{label}: the golden text has no anchor"));
    };
    if outcome.anchor != anchor {
        return Err(format!(
            "{label}: anchor field {} is not {anchor}",
            outcome.anchor
        ));
    }
    if !ANCHORS.contains(&anchor) {
        return Err(format!("{label}: anchor {anchor} is not in ANCHORS"));
    }
    Ok(())
}

/// Render the report a corpus row names and compare every output form with the golden.
fn check_row(case: &Case) -> Result<(), String> {
    let report = load_report(&case.report);
    let expected = expected_text(case, &report);
    let label = format!("row {} ({})", case.row, case.report);
    let Some(outcome) = explain_report(&report) else {
        return Err(format!("{label}: no plain lines"));
    };
    check_outcome(&label, &outcome, &expected)
}

fn assert_row(row_id: &str) {
    if let Err(problem) = check_row(&find_case(row_id)) {
        panic!("{problem}");
    }
}

fn early_cases() -> Vec<EarlyCase> {
    serde_json::from_str(&read_repo_file(&format!("{EXPLAIN_FIXTURES}/early.json")))
        .expect("early.json must be a JSON array of {cause, text}")
}

const STORAGE_CLASSES: [StorageClass; 11] = [
    StorageClass::InvalidPath,
    StorageClass::UnsafeDirectory,
    StorageClass::JournalExists,
    StorageClass::CreateFailed,
    StorageClass::WriteFailed,
    StorageClass::SyncFailed,
    StorageClass::ReadFailed,
    StorageClass::InvalidRecord,
    StorageClass::UnsafeTarget,
    StorageClass::FinalizeFailed,
    StorageClass::Unknown,
];

const STORAGE_PHASES: [StoragePhase; 5] = [
    StoragePhase::BeforeLaunch,
    StoragePhase::LaunchFailed,
    StoragePhase::StartUnfinished,
    StoragePhase::EndedWhileStarting,
    StoragePhase::AfterLaunch,
];

/// The corpus word for a storage kind. Exhaustive, so a new kind cannot go unnamed.
const fn storage_word(kind: StorageClass) -> &'static str {
    match kind {
        StorageClass::InvalidPath => "invalid_path",
        StorageClass::UnsafeDirectory => "unsafe_directory",
        StorageClass::JournalExists => "journal_exists",
        StorageClass::CreateFailed => "create_failed",
        StorageClass::WriteFailed => "write_failed",
        StorageClass::SyncFailed => "sync_failed",
        StorageClass::ReadFailed => "read_failed",
        StorageClass::InvalidRecord => "invalid_record",
        StorageClass::UnsafeTarget => "unsafe_target",
        StorageClass::FinalizeFailed => "finalize_failed",
        StorageClass::Unknown => "unknown",
    }
}

const fn phase_word(phase: StoragePhase) -> &'static str {
    match phase {
        StoragePhase::BeforeLaunch => "before_launch",
        StoragePhase::LaunchFailed => "launch_failed",
        StoragePhase::StartUnfinished => "start_unfinished",
        StoragePhase::EndedWhileStarting => "ended_while_starting",
        StoragePhase::AfterLaunch => "after_launch",
    }
}

/// The corpus word for a cause's variant. Exhaustive, so a new variant cannot go unnamed.
const fn variant_word(cause: EarlyCause) -> &'static str {
    match cause {
        EarlyCause::CommandLine { .. } => "command_line",
        EarlyCause::ClientReadyDescriptor => "client_ready_descriptor",
        EarlyCause::Configuration => "configuration",
        EarlyCause::UnsupportedPlatform => "unsupported_platform",
        EarlyCause::FootprintUnavailable => "footprint_unavailable",
        EarlyCause::RunIdentity => "run_identity",
        EarlyCause::SupervisorSetup => "supervisor_setup",
        EarlyCause::AutoLimitUnavailable => "auto_limit_unavailable",
        EarlyCause::AutoLimitBelowFloor => "auto_limit_below_floor",
        EarlyCause::Storage { .. } => "storage",
    }
}

/// Parse an `early.json` cause: `command_line [flag]`, a bare variant word, or
/// `storage <kind> <phase>`.
fn parse_cause(words: &str) -> EarlyCause {
    let parts: Vec<&str> = words.split(' ').collect();
    match parts.as_slice() {
        ["command_line"] => EarlyCause::CommandLine { flag: None },
        ["command_line", flag] => EarlyCause::CommandLine {
            flag: Some(Box::leak((*flag).to_owned().into_boxed_str())),
        },
        ["storage", kind, phase] => EarlyCause::Storage {
            kind: *STORAGE_CLASSES
                .iter()
                .find(|class| storage_word(**class) == *kind)
                .unwrap_or_else(|| panic!("early.json: unknown storage kind {kind}")),
            phase: *STORAGE_PHASES
                .iter()
                .find(|candidate| phase_word(**candidate) == *phase)
                .unwrap_or_else(|| panic!("early.json: unknown storage phase {phase}")),
        },
        [word] => [
            EarlyCause::ClientReadyDescriptor,
            EarlyCause::Configuration,
            EarlyCause::UnsupportedPlatform,
            EarlyCause::FootprintUnavailable,
            EarlyCause::RunIdentity,
            EarlyCause::SupervisorSetup,
            EarlyCause::AutoLimitUnavailable,
            EarlyCause::AutoLimitBelowFloor,
        ]
        .into_iter()
        .find(|cause| variant_word(*cause) == *word)
        .unwrap_or_else(|| panic!("early.json: unknown cause {word}")),
        _ => panic!("early.json: cannot parse cause {words:?}"),
    }
}

#[test]
fn footprint_breach_matches_the_tutorial_report() {
    // Bug: "above" instead of "at or above", or 6 499 453 256 rounded to 6.06 GiB.
    assert_row("75-footprint");
}

#[test]
fn acknowledged_checkpoint_names_its_request_id() {
    // Bug: the resume step dropped, or request_id taken from the signal instead of `checkpoint`.
    assert_row("75-footprint-acknowledged");
}

#[test]
fn timed_out_checkpoint_offers_a_conditional_resume() {
    // Bug: timed_out rendered like acknowledged (an unconditional resume), or its clause dropped.
    assert_row("75-footprint-timed-out");
}

#[test]
fn cancelled_checkpoint_says_the_request_was_not_delivered() {
    // Bug: the cancelled clause dropped, or an undelivered endpoint record read as "asked the
    // command to save a checkpoint".
    assert_row("75-footprint-cancelled");
}

#[test]
fn footprint_without_a_breach_transition_omits_the_number() {
    // Bug: an invented footprint value (e.g. the warning transition's 100 B) when no breach
    // transition exists.
    assert_row("75-footprint-no-breach");
}

#[test]
fn emergency_kill_names_the_threshold_and_no_grace() {
    // Bug: footprint checked before emergency, so an emergency KILL (reason footprint) reads as a
    // graceful TERM.
    assert_row("75-emergency");
}

#[test]
fn wall_time_names_the_configured_limit() {
    // Bug: the duration formatter prints `0.0 s` for 30 ms.
    assert_row("75-wall");
}

#[test]
fn kill_after_term_reports_the_gap() {
    // Bug: the gap computed from outcome.at_ms (1 100) instead of the TERM record (31), giving 1.1 s.
    assert_row("75-wall-kill");
}

#[test]
fn parent_exit_points_to_detach() {
    // Bug: the default-flag explanation dropped, or parent_exited_at_ms replaced by the TERM time.
    assert_row("75-parent");
}

#[test]
fn escalated_forwarded_signal_is_not_called_a_footprint_breach() {
    // Bug: external_signal checked after footprint, or the absent checkpoint.reason misread as a
    // footprint cause.
    assert_row("75-ignored-signal");
}

#[test]
fn ended_group_kill_is_not_called_an_ignored_signal() {
    // Bug: 75-ignored-signal matched on a SIGKILL external_signal record without
    // `result: delivered`, so a KILL that found the group already ended reads as "still running".
    assert_row("75-unrecorded-kill-on-ended");
}

#[test]
fn undelivered_forward_is_not_called_ignored() {
    // Bug: 75-ignored-signal matched without a delivered forward before the KILL, so a forward
    // that found the group already ended reads as a signal the command ignored.
    assert_row("75-unrecorded-forward-missing");
}

#[test]
fn survivor_cleanup_kill_explains_child_processes() {
    // Bug: root_exit_cleanup falls through to 75-unrecorded.
    assert_row("75-survivors");
}

#[test]
fn pre_reason_reports_fall_back_honestly() {
    // Bug: a reason-less 0.1.0 report guessed as a footprint breach.
    assert_row("75-unrecorded");
}

#[test]
fn launch_setting_refusal_names_cwd_env_and_a_new_report_name() {
    // Bug: the new --report advice dropped, so the retry trap of 0.3.0 returns.
    assert_row("64-launch-setting");
}

#[test]
fn zero_sample_failure_reads_as_a_start_failure() {
    // Bug: the `samples empty and at_ms == 0` rule dropped, so a start failure falls through to
    // the internal-failure (or readings-lost) sentence.
    assert_row("70-while-starting");
}

#[test]
fn run_observation_loss_says_the_command_was_stopped() {
    // Bug: the run/observe branch inverted, or the last reading taken from an advisory field
    // instead of the last sample's aggregate footprint.
    assert_row("70-readings-lost-run");
}

#[test]
fn observe_observation_loss_says_the_command_may_still_run() {
    // Bug: observe told the user the command was stopped (the consequential lie).
    assert_row("70-readings-lost-observe");
}

#[test]
fn failed_kill_names_the_signal_and_a_live_member() {
    // Bug: the signal named from the first group record (the delivered TERM) instead of the
    // failing KILL, or the pre-0.3.1 claim "while a member of it was still alive" kept.
    assert_row("70-not-delivered");
}

#[test]
fn failed_kill_result_has_its_own_words() {
    // Bug: the `failed` result rendered with the permission_denied words, or not matched at all.
    assert_row("70-not-delivered-failed");
}

#[test]
fn unclassified_supervisor_failure_invents_no_cause() {
    // Bug: the fallback names observation loss, a run-mode supervisor_error transition is read as
    // lost readings, or the KILL with process_missing is counted as a delivery failure (0102).
    // The fixture has no samples and at_ms 120, so it also pins the at_ms half of 70-while-starting.
    assert_row("70-internal");
}

#[test]
fn not_executable_and_not_found_have_distinct_fixes() {
    // Bug: the 126 and 127 rows swapped.
    assert_row("126");
    assert_row("127");
}

#[test]
fn forwarded_signal_is_attributed_to_mlx_guard_passing_it_on() {
    // Bug: the n = 9 "second terminal signal" variant missing, or the signal named from the first
    // forwarded record (SIGINT) instead of the one the command ended on.
    assert_row("S-passed-on");
    assert_row("S-passed-on-9");
}

#[test]
fn foreign_sigkill_is_not_claimed_by_mlx_guard() {
    // Bug: a match on reason alone (the delivered SIGTERM forward) or on signal number without
    // `result: delivered` (the SIGKILL forward found process_missing), so mlx-guard claims a kill
    // it never delivered.
    assert_row("S-elsewhere-9");
}

#[test]
fn foreign_non_kill_signal_points_to_the_crash_report() {
    // Bug: the SIGKILL memory-shortage clause printed for every foreign signal (n == 9 check lost).
    assert_row("S-elsewhere-11");
}

#[test]
fn child_exit_75_is_not_called_an_intervention() {
    // Bug: collision codes compared against the wrong set, or a command's own 75 read as a policy
    // intervention.
    assert_row("C-collision");
    let mut report = load_explain_fixture("report-C-collision.json");
    for code in [64_u8, 70, 74, 75, 126, 127] {
        report.outcome.kind = TerminalKind::ChildExited { code };
        report.outcome.child_status = Some(ChildStatus::Exited { code });
        report
            .validate()
            .expect("the edited report must stay valid");
        let outcome = explain_report(&report)
            .unwrap_or_else(|| panic!("child code {code}: a colliding code must be explained"));
        assert_eq!(
            outcome.anchor, "a-code-mlx-guard-also-uses",
            "child code {code}"
        );
        let text = outcome.text();
        assert_eq!(
            text.split('\n').next(),
            Some(
                format!(
                    "what happened: the command itself exited with code {code}, and mlx-guard \
                     did not intervene; {code} is also one of mlx-guard's own exit codes."
                )
                .as_str()
            ),
            "child code {code}"
        );
    }
}

#[test]
fn quiet_outcomes_have_no_explanation() {
    // Bug: a line printed on success, or on a command's own code next to a reserved one
    // (63/65, 69/71, 73/76, 125/128).
    for name in ["report-quiet-exit-0.json", "report-quiet-child-1.json"] {
        let report = load_explain_fixture(name);
        assert_eq!(
            explain_report(&report).as_ref().map(PlainOutcome::text),
            None,
            "{name}"
        );
    }
    let mut report = load_explain_fixture("report-quiet-child-1.json");
    for code in [0_u8, 1, 2, 23, 63, 65, 69, 71, 73, 76, 125, 128, 255] {
        report.outcome.kind = TerminalKind::ChildExited { code };
        report.outcome.child_status = Some(ChildStatus::Exited { code });
        report
            .validate()
            .expect("the edited report must stay valid");
        assert_eq!(
            explain_report(&report).as_ref().map(PlainOutcome::text),
            None,
            "child code {code}"
        );
    }
}

#[test]
fn observe_refused_forward_is_not_called_a_failed_delivery() {
    // Bug: rule 5.2 without its `run` mode check, so an observe report with a refused forward
    // claims a delivery failure that observe never had.
    assert_row("70-readings-lost-observe-refused");
}

#[test]
fn kill_on_an_ended_group_is_not_called_escalation() {
    // Bug: the KILL suffix keyed on record order alone, so a process_missing KILL after a TERM
    // reads "and SIGKILL … later". The second case is the same shape with the KILL delivered.
    assert_row("75-wall-kill-on-ended");
    assert_row("75-wall-second-signal-kill");
}

#[test]
fn term_on_an_ended_group_is_not_called_a_stop() {
    // Bug: "sent SIGTERM" printed for a TERM record whose result is process_missing, with and
    // without a delivered checkpoint request before it.
    assert_row("75-wall-checkpoint-then-ended");
    assert_row("75-wall-already-ended");
}

#[test]
fn anchors_match_python() {
    // Bug: an anchor added, renamed or reordered in ANCHORS without the shared anchors.txt that
    // the Python mirror and the troubleshooting-page test read.
    let listed_text = read_repo_file(&format!("{EXPLAIN_FIXTURES}/anchors.txt"));
    let listed: Vec<&str> = listed_text.lines().collect();
    assert_eq!(ANCHORS, listed.as_slice());
    let mut unique = listed.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        listed.len(),
        "anchors.txt lists an anchor twice"
    );
}

#[test]
fn every_explain_fixture_is_a_valid_report() {
    // Bug: a synthetic fixture that breaks a schema-v1 invariant, so a golden pins a report the
    // binary can never write.
    let directory = repo_root().join(EXPLAIN_FIXTURES);
    let mut checked = 0_usize;
    for entry in fs::read_dir(&directory).expect("the explain fixture directory must exist") {
        let path = entry.expect("readable directory entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 file name")
            .to_owned();
        if name.starts_with("report-") && path.extension().is_some_and(|ext| ext == "json") {
            load_explain_fixture(&name);
            checked += 1;
        }
    }
    assert!(checked > 0, "no report-*.json fixtures found");
    for case in cases() {
        load_report(&case.report);
    }
}

#[test]
fn every_corpus_case_renders_its_text() {
    // Bug: a case added to cases.json without a row test above is never checked on the Rust side.
    // Every mismatch is listed, so a red run names each row that drifted, not only the first.
    let problems: Vec<String> = cases()
        .iter()
        .filter_map(|case| check_row(case).err())
        .collect();
    assert!(
        problems.is_empty(),
        "{} corpus rows differ:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[test]
fn emergency_kill_on_an_ended_command_is_not_called_sent() {
    // Bug: the emergency row reads the first KILL record without `result: delivered`, so a KILL
    // that found the command already ended reads "sent SIGKILL to the command's process group".
    assert_row("75-emergency-kill-missing");
}

#[test]
fn survivor_kill_on_ended_processes_is_not_called_sent() {
    // Bug: the survivors row reads the first KILL record without `result: delivered`, so a KILL
    // that found the left-behind processes already gone reads "sent SIGKILL to them".
    assert_row("75-survivors-kill-missing");
}

#[test]
fn kill_after_an_ended_term_is_its_own_clause() {
    // Bug: the escalation suffix appended to "the command ended before the SIGTERM … could reach
    // it" (or to "found the command already ended when it sent SIGTERM"), so the sentence says
    // the KILL found the command ended too, the opposite of its `delivered` record.
    assert_row("75-wall-checkpoint-ended-second-signal");
    assert_row("75-unrecorded-forward-missing");
}

#[test]
fn unrecorded_fallback_with_recorded_reasons_claims_no_missing_cause() {
    // Bug: the fallback says "this report does not record which limit caused it" for a report
    // whose records do carry a reason, and sends the reader to look for a limit nobody crossed.
    // `75-unrecorded` (no reason anywhere) keeps the old sentence: the other half of the pair.
    assert_row("75-unrecorded-kill-on-ended");
    assert_row("75-unrecorded");
}

#[test]
fn equal_renders_of_different_values_are_printed_in_bytes() {
    // Bug: the equal-render rule missing, so a breach reads "6.00 GiB, at or above the 6.00 GiB
    // limit" for two different numbers.
    assert_row("75-footprint-equal-render");
}

#[test]
fn partial_artifact_report_uses_the_kind_fallback() {
    // Bug: the defensive 74 row returns nothing, or invents a storage detail it never had.
    assert_row("74-kind-fallback");
}

#[test]
fn every_last_reading_has_its_own_words() {
    // Bug: an observation status mapped to another status's words (or to none), so the reader
    // is told the wrong reason the readings stopped.
    for row in [
        "70-readings-lost-observe",
        "70-readings-lost-run",
        "70-readings-lost-observe-malformed",
        "70-readings-lost-observe-clock",
        "70-readings-lost-observe-internal",
        "70-readings-lost-observe-stale",
        "70-readings-lost-observe-unavailable",
        "70-readings-lost-observe-unknown",
    ] {
        assert_row(row);
    }
}

#[test]
fn every_early_cause_renders_its_text() {
    // Bug: a report-less sentence drifts from the shared corpus that the troubleshooting page is
    // checked against (a word, a dropped detail, a routing change between kinds or phases).
    let entries = early_cases();
    assert!(!entries.is_empty(), "early.json lists no causes");
    let problems: Vec<String> = entries
        .iter()
        .filter_map(|entry| {
            let outcome = explain_early(parse_cause(&entry.cause));
            let expected = entry.text.replace("{version}", VERSION);
            check_outcome(&format!("cause {}", entry.cause), &outcome, &expected).err()
        })
        .collect();
    assert!(
        problems.is_empty(),
        "{} early causes differ:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[test]
fn early_corpus_covers_every_cause_and_form() {
    // Bug: a cause, a command-line form, or a storage kind in a phase left out of early.json, so
    // its sentence (and the page's quote of it) is never checked against the binary.
    let listed: Vec<EarlyCause> = early_cases()
        .iter()
        .map(|entry| parse_cause(&entry.cause))
        .collect();
    let mut required = vec![
        EarlyCause::CommandLine { flag: None },
        EarlyCause::CommandLine {
            flag: Some("--max-footprint"),
        },
        EarlyCause::CommandLine {
            flag: Some("<COMMAND>"),
        },
        EarlyCause::ClientReadyDescriptor,
        EarlyCause::Configuration,
        EarlyCause::UnsupportedPlatform,
        EarlyCause::FootprintUnavailable,
        EarlyCause::RunIdentity,
        EarlyCause::SupervisorSetup,
        EarlyCause::AutoLimitUnavailable,
        EarlyCause::AutoLimitBelowFloor,
    ];
    for kind in STORAGE_CLASSES {
        for phase in [StoragePhase::BeforeLaunch, StoragePhase::AfterLaunch] {
            required.push(EarlyCause::Storage { kind, phase });
        }
    }
    for kind in [StorageClass::JournalExists, StorageClass::Unknown] {
        required.push(EarlyCause::Storage {
            kind,
            phase: StoragePhase::LaunchFailed,
        });
    }
    // The two phases between launch and supervision: one kind with detail words, one without.
    for phase in [
        StoragePhase::StartUnfinished,
        StoragePhase::EndedWhileStarting,
    ] {
        for kind in [StorageClass::InvalidRecord, StorageClass::Unknown] {
            required.push(EarlyCause::Storage { kind, phase });
        }
    }
    let missing: Vec<String> = required
        .iter()
        .filter(|cause| !listed.contains(cause))
        .map(|cause| format!("{} {cause:?}", variant_word(*cause)))
        .collect();
    assert!(missing.is_empty(), "early.json lacks: {missing:?}");
    for (index, cause) in listed.iter().enumerate() {
        assert!(
            !listed[index + 1..].contains(cause),
            "early.json lists {cause:?} twice"
        );
    }
}

#[test]
fn no_sentence_names_the_run_id_or_the_executable() {
    // Bug: a template interpolates run.run_id, run.executable_basename or the correlation hash
    // (the synthetic fixtures carry the canary basename `canary_exe_7f3a`).
    for case in cases() {
        let report = load_report(&case.report);
        let text = explain_report(&report)
            .unwrap_or_else(|| panic!("row {}: no plain lines", case.row))
            .text();
        assert!(
            !text.contains(&report.run.run_id),
            "row {}: run id",
            case.row
        );
        assert!(
            !text.contains(&report.run.executable_basename),
            "row {}: executable name",
            case.row
        );
        if let Some(hash) = &report.run.correlation_hash {
            assert!(
                !text.contains(hash.as_str()),
                "row {}: correlation hash",
                case.row
            );
        }
    }
}

#[test]
fn report_without_source_keeps_the_explicit_wording() {
    // Bug: the automatic wording keyed on the prelaunch record (every 0.5.0 typed-size report
    // has one) or on "source is not explicit" (an absent source means explicit), so a 0.5.0
    // report read by this release says the limit was chosen automatically.
    for (relative, row) in [
        (FOOTPRINT_REPORT, "75-footprint"),
        (EMERGENCY_REPORT, "75-emergency"),
        (
            "crates/mlx-guard-core/tests/fixtures/report-v1-resume.json",
            "75-footprint-no-breach",
        ),
    ] {
        let case = find_case(row);
        // "a_later_source" is a source written by a later release: still the limit you set.
        let later: LimitSource = serde_json::from_str("\"a_later_source\"").unwrap();
        for source in [None, Some(LimitSource::Explicit), Some(later)] {
            let report = explicit_with_prelaunch(relative, source);
            assert_eq!(
                rendered(&report),
                expected_text(&case, &report),
                "row {row}, source {source:?}"
            );
        }
    }
}

#[test]
fn footprint_stop_at_an_automatic_limit_says_mlx_guard_chose_it() {
    // Bug: the footprint row has no automatic branch (an automatic run reads "the limit you
    // set" and "raise --max-footprint"), or its "what to do" drops the advice to pass a size
    // with headroom above an observed peak. The explicit half of this pair is the test above.
    let report = as_automatic(FOOTPRINT_REPORT, AutoLimitRule::NotWired75Percent);
    assert_eq!(
        rendered(&report),
        "what happened: the OS-accounted footprint reached 6.05 GiB, at or above the 6.00 GiB \
         automatic limit mlx-guard chose at launch, on 2 samples in a row, so mlx-guard sent \
         SIGTERM to the command's process group at 40.0 s.\n\
         what to do: the automatic limit came from this Mac's memory at launch, not from what \
         the command needs: observe the command to see its peak, then pass a size with headroom \
         above that peak to --max-footprint, or fix the growth. More: \
         https://github.com/IonDen/mlx-guard/blob/v0.2.0/docs/TROUBLESHOOTING.md\
         #75-the-footprint-limit"
    );
}

#[test]
fn emergency_kill_at_an_automatic_limit_says_mlx_guard_chose_it() {
    // Bug: the emergency row has no automatic branch, so an automatic run reads "your 6.00 GiB
    // limit" and "raise --max-footprint" for a number the user never chose.
    let report = as_automatic(EMERGENCY_REPORT, AutoLimitRule::NotWired75Percent);
    assert_eq!(
        rendered(&report),
        "what happened: one sample of the OS-accounted footprint, 7.00 GiB, was at or above the \
         emergency threshold of 6.60 GiB, which is 614.40 MiB more than the 6.00 GiB automatic \
         limit mlx-guard chose at launch, so mlx-guard sent SIGKILL to the command's process \
         group at once at 12.3 s, with no checkpoint and no grace period.\n\
         what to do: find what allocates that much between samples, or pass a size to \
         --max-footprint if that peak is expected; a shorter --sample-interval sees growth \
         sooner. More: https://github.com/IonDen/mlx-guard/blob/v0.1.0/docs/TROUBLESHOOTING.md\
         #75-emergency-kill"
    );
}

#[test]
fn a_stop_line_reads_the_source_not_the_rule() {
    // Bug: the stop lines branch on the rule (an id a later release writes falls back to "the
    // limit you set") or print its share (an unknown rule's share renders as "0%").
    for relative in [FOOTPRINT_REPORT, EMERGENCY_REPORT] {
        let known = rendered(&as_automatic(relative, AutoLimitRule::NotWired75Percent));
        let unknown = rendered(&as_automatic(relative, AutoLimitRule::Unknown));
        assert_eq!(unknown, known, "{relative}");
        assert!(!unknown.contains('%'), "{unknown}");
    }
}

#[test]
fn every_automatic_fixture_has_its_corpus_row() {
    // Bug: an automatic stop fixture with no cases.json row, so its sentence (and the page's
    // quote of it) is checked in neither language.
    let all = cases();
    for name in AUTO_FIXTURES {
        let rows: Vec<&Case> = all
            .iter()
            .filter(|case| case.report == format!("{EXPLAIN_FIXTURES}/{name}"))
            .collect();
        assert_eq!(rows.len(), 1, "cases.json rows for {name}");
        if let Err(problem) = check_row(rows[0]) {
            panic!("{problem}");
        }
        let report = load_explain_fixture(name);
        assert_eq!(
            report.configuration.max_footprint_source,
            Some(LimitSource::Auto),
            "{name}"
        );
        assert!(
            rows[0]
                .text
                .contains(" automatic limit mlx-guard chose at launch"),
            "{name}: {}",
            rows[0].text
        );
    }
}

#[test]
fn max_footprint_hint_offers_auto() {
    // Bug: the --max-footprint "what to do" line keeps the 0.5.0 text, so a user who left the
    // flag out is never told that `auto` exists.
    assert_eq!(
        explain_early(EarlyCause::CommandLine {
            flag: Some("--max-footprint"),
        })
        .text(),
        format!(
            "what happened: the command line was not accepted (--max-footprint), so nothing was \
             launched and no report was written.\n\
             what to do: fix --max-footprint as the error above says: give a size such as 16GiB, \
             or auto to let mlx-guard pick one from this Mac's memory just before launch, then \
             run the command again. More: https://github.com/IonDen/mlx-guard/blob/v{VERSION}/\
             docs/TROUBLESHOOTING.md#64-the-command-line"
        )
    );
}

#[test]
fn no_usable_automatic_limit_has_its_own_plain_lines() {
    // Bug: the refusal reuses the generic configuration row ("fix that setting"), which gives
    // a user of `auto` nothing to fix, or links the command-line section.
    let outcome = explain_early(EarlyCause::AutoLimitUnavailable);
    assert_eq!(
        outcome.text(),
        format!(
            "what happened: --max-footprint auto could not pick a limit, because macOS gave no \
             usable memory number just before launch, so nothing was launched and no report was \
             written.\n\
             what to do: pass a size instead, such as --max-footprint 16GiB, taken from a peak \
             you observed, then run the command again. More: \
             https://github.com/IonDen/mlx-guard/blob/v{VERSION}/docs/TROUBLESHOOTING.md\
             #64-the-automatic-limit"
        )
    );
    assert_eq!(outcome.anchor, "64-the-automatic-limit");
}

#[test]
fn automatic_limit_below_the_floor_names_the_minimum() {
    // Bug: the below-floor refusal reuses the no-number plain line ("macOS gave no usable memory
    // number"), which is false when macOS gave a number and the limit from it was too small.
    let outcome = explain_early(EarlyCause::AutoLimitBelowFloor);
    assert_eq!(
        outcome.text(),
        format!(
            "what happened: --max-footprint auto could not pick a limit, because the limit it \
             would pick from this Mac's memory just before launch is under the 1 GiB minimum, so \
             nothing was launched and no report was written.\n\
             what to do: pass a size instead, such as --max-footprint 16GiB, taken from a peak \
             you observed, then run the command again. More: \
             https://github.com/IonDen/mlx-guard/blob/v{VERSION}/docs/TROUBLESHOOTING.md\
             #64-the-automatic-limit"
        )
    );
    assert_eq!(outcome.anchor, "64-the-automatic-limit");
}

#[test]
fn automatic_limit_anchor_follows_the_launch_setting_anchor() {
    // Bug: the new anchor missing from ANCHORS, or placed elsewhere in the list that Python and
    // the page test mirror in order.
    let position = ANCHORS
        .iter()
        .position(|anchor| *anchor == "64-a-launch-setting")
        .expect("64-a-launch-setting is listed");
    assert_eq!(ANCHORS.get(position + 1), Some(&"64-the-automatic-limit"));
}
