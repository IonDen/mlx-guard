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
use mlx_guard_core::{ChildStatus, ReportV1, TerminalKind, VERSION};
use serde::Deserialize;

const EXPLAIN_FIXTURES: &str = "crates/mlx-guard-core/tests/fixtures/explain";

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

const STORAGE_PHASES: [StoragePhase; 4] = [
    StoragePhase::BeforeLaunch,
    StoragePhase::LaunchFailed,
    StoragePhase::StartUnfinished,
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
