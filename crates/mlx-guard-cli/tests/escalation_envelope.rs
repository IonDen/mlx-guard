#![cfg(target_os = "macos")]
//! Pure core of the escalation-envelope measurement instrument.
//!
//! `mod extract` turns a parsed schema-v1 report's signal, checkpoint, and outcome records into
//! typed escalation marks and the intervals between them, captures host provenance for a
//! measurement run, and serializes a verdict-free JSON artifact. This file adds only the pure
//! extraction core and its unit tests, built and watched failing before `mod extract` existed;
//! the `#[ignore]`d real-process capture that drives real supervised runs through this module
//! lands in a later change.

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use mlx_guard_core::{
    CheckpointRecord, CheckpointStatus, Observed, SignalRecord, SignalResult, SignalTarget,
    TerminalKind, TerminalOutcome, checkpoint_signal_usr1,
};

use extract::{
    EnvelopeProvenance, EscalationMarks, IntervalSummary, ScenarioSummary, capture_provenance,
    extract_escalation_marks, intervals, parse_loadavg, write_envelope_artifact,
};

fn delivered(at_ms: u64, signal: u8, target: SignalTarget) -> SignalRecord {
    SignalRecord {
        at_ms,
        signal,
        target,
        result: SignalResult::Delivered,
        reason: None,
    }
}

fn outcome_at(kind: TerminalKind, at_ms: u64) -> TerminalOutcome {
    TerminalOutcome {
        at_ms,
        kind,
        final_footprint_bytes: Observed::Unknown,
        child_status: None,
        owned_group_survivors: None,
        parent_exited_at_ms: None,
    }
}

#[test]
fn ack_time_comes_only_from_an_acknowledged_checkpoint_record() {
    // Catches ignoring the checkpoint status and reading `at_ms` unconditionally.
    let signals = [delivered(
        100,
        checkpoint_signal_usr1().get(),
        SignalTarget::CooperativeEndpoint,
    )];
    let checkpoint = CheckpointRecord {
        status: CheckpointStatus::AcknowledgedUnverifiedDurability,
        at_ms: Some(340),
    };
    let outcome = outcome_at(TerminalKind::PolicyIntervention, 1_000);

    let marks = extract_escalation_marks(&signals, Some(&checkpoint), &outcome);

    assert_eq!(marks.checkpoint_requested_at_ms, Some(100));
    assert_eq!(marks.checkpoint_ack_at_ms, Some(340));
    assert_eq!(intervals(&marks).request_to_ack_ms, Some(240));
}

#[test]
fn timed_out_checkpoint_yields_no_ack_interval() {
    // Catches reading `at_ms` off a checkpoint record without checking its status — the exact
    // trap the schema sets, since `TimedOut` still carries a populated `at_ms`.
    let signals = [delivered(
        100,
        checkpoint_signal_usr1().get(),
        SignalTarget::CooperativeEndpoint,
    )];
    let checkpoint = CheckpointRecord {
        status: CheckpointStatus::TimedOut,
        at_ms: Some(1_100),
    };
    let outcome = outcome_at(TerminalKind::PolicyIntervention, 1_200);

    let marks = extract_escalation_marks(&signals, Some(&checkpoint), &outcome);

    assert_eq!(marks.checkpoint_ack_at_ms, None);
    assert_eq!(intervals(&marks).request_to_ack_ms, None);
}

#[test]
fn first_delivered_term_and_kill_are_the_marks() {
    // Catches taking the last matching signal record instead of the first.
    let signals = [
        delivered(500, 15, SignalTarget::OwnedProcessGroup),
        delivered(900, 15, SignalTarget::OwnedProcessGroup),
        delivered(1_500, 9, SignalTarget::OwnedProcessGroup),
    ];
    let outcome = outcome_at(TerminalKind::ChildSignaled { signal: 9 }, 1_600);

    let marks = extract_escalation_marks(&signals, None, &outcome);

    assert_eq!(marks.term_at_ms, Some(500));
    assert_eq!(marks.kill_at_ms, Some(1_500));
    let computed = intervals(&marks);
    assert_eq!(computed.term_to_quiet_ms, Some(1_100));
    assert_eq!(computed.kill_to_quiet_ms, Some(100));
}

#[test]
fn undelivered_signals_are_not_marks() {
    // Catches counting a signal attempt whose delivery result was not `Delivered`.
    let signals = [SignalRecord {
        at_ms: 500,
        signal: 15,
        target: SignalTarget::OwnedProcessGroup,
        result: SignalResult::ProcessMissing,
        reason: None,
    }];
    let outcome = outcome_at(TerminalKind::PolicyIntervention, 600);

    let marks = extract_escalation_marks(&signals, None, &outcome);

    assert_eq!(marks.term_at_ms, None);
}

#[test]
fn cooperative_endpoint_usr1_is_the_request_mark_not_group_signals() {
    // Catches matching the USR1 signal number regardless of its target.
    let signals = [delivered(
        100,
        checkpoint_signal_usr1().get(),
        SignalTarget::OwnedProcessGroup,
    )];
    let outcome = outcome_at(TerminalKind::PolicyIntervention, 200);

    let marks = extract_escalation_marks(&signals, None, &outcome);

    assert_eq!(marks.checkpoint_requested_at_ms, None);
}

#[test]
fn supervisor_failure_outcome_is_not_quiet() {
    // Catches publishing "the supervisor gave up" as if it were a clean quiet moment.
    let outcome = outcome_at(TerminalKind::SupervisorFailure, 9_000);

    let marks = extract_escalation_marks(&[], None, &outcome);

    assert_eq!(marks.quiet_at_ms, None);
}

#[test]
fn policy_intervention_outcome_is_quiet() {
    // Catches a quiet-kind gate that over-narrows and excludes the exit-75 case every scenario
    // in this instrument ends in, which would empty the whole instrument.
    let outcome = outcome_at(TerminalKind::PolicyIntervention, 1_600);

    let marks = extract_escalation_marks(&[], None, &outcome);

    assert_eq!(marks.quiet_at_ms, Some(1_600));
}

#[test]
fn empty_interval_set_summarizes_to_none() {
    // Catches an index computed against an empty Some()-set (`0 - 1` underflow).
    let summary = IntervalSummary::from_raw(vec![None, None, None]);

    assert_eq!(summary.p95_ms, None);
    assert_eq!(summary.maximum_ms, None);
    assert_eq!(summary.missing, 3);
}

#[test]
fn negative_interval_is_none_not_wraparound() {
    // Catches `-` replacing `checked_sub` and silently wrapping an unsigned subtraction.
    let marks = EscalationMarks {
        checkpoint_requested_at_ms: None,
        checkpoint_ack_at_ms: None,
        term_at_ms: Some(500),
        kill_at_ms: None,
        quiet_at_ms: Some(100),
    };

    let computed = intervals(&marks);

    assert_eq!(computed.term_to_quiet_ms, None);
}

#[test]
fn loadavg_line_parses_the_three_figures() {
    // Catches a parser that mishandles `sysctl -n vm.loadavg`'s brace-delimited shape, or that
    // accepts garbage instead of rejecting it.
    assert_eq!(
        parse_loadavg("{ 1.78 2.01 2.05 }"),
        Some([1.78, 2.01, 2.05])
    );
    assert_eq!(parse_loadavg("not a loadavg line"), None);
}

#[test]
fn p95_of_twenty_is_the_second_largest() {
    // Catches an off-by-one in the p95 index formula.
    let raw: Vec<Option<u64>> = (1..=20).map(Some).collect();

    let summary = IntervalSummary::from_raw(raw);

    assert_eq!(summary.p95_ms, Some(19));
    assert_eq!(summary.maximum_ms, Some(20));
    assert_eq!(summary.missing, 0);
}

fn unique_scratch_directory(tag: &str) -> std::path::PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "mlx-guard-envelope-{tag}-{}-{timestamp}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    directory
}

fn assert_no_verdict_fields(value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            assert!(
                !map.contains_key("passed"),
                "found a `passed` field: {value}"
            );
            assert!(
                !map.contains_key("target"),
                "found a `target` field: {value}"
            );
            for nested in map.values() {
                assert_no_verdict_fields(nested);
            }
        }
        serde_json::Value::Array(items) => {
            for nested in items {
                assert_no_verdict_fields(nested);
            }
        }
        _ => {}
    }
}

#[test]
fn artifact_has_no_verdict_fields() {
    // Catches a grading field (`passed`, `target`) creeping into the artifact schema.
    let directory = unique_scratch_directory("verdict");
    let path = directory.join("envelope.json");
    let provenance = EnvelopeProvenance {
        profile: "scenario-a".to_owned(),
        host: vec!["Chip: Apple M1 Max".to_owned()],
        cpu_count: 10,
        memory_bytes: 34_359_738_368,
        load_at_start: [1.0, 1.0, 1.0],
        git_commit: "deadbeef".to_owned(),
        git_status_porcelain: String::new(),
        captured_at_utc: "2026-08-26T00:00:00Z".to_owned(),
        resolution_ms: 10,
    };
    let scenarios = [ScenarioSummary {
        name: "scenario-a".to_owned(),
        repetitions: 1,
        request_to_ack: Some(IntervalSummary::from_raw(vec![Some(240)])),
        term_to_quiet: None,
        kill_to_quiet: None,
        timed_out: 0,
        escalated_to_kill: 0,
        load_at_start_per_repetition: vec![[1.0, 1.0, 1.0]],
    }];

    write_envelope_artifact(&path, &provenance, &scenarios).unwrap();

    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_no_verdict_fields(&value);
    fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn sanitized_host_omits_unique_machine_identifiers() {
    // Catches shipping an unfiltered `system_profiler` line, leaking a serial/UUID/UDID — the
    // privacy test the review demanded, run for real against this host.
    const ALLOWED_PREFIXES: [&str; 5] = [
        "Model Name:",
        "Model Identifier:",
        "Chip:",
        "Total Number of Cores:",
        "Memory:",
    ];

    let provenance = capture_provenance("privacy-check", 10);

    assert!(!provenance.host.is_empty());
    for line in &provenance.host {
        assert!(
            ALLOWED_PREFIXES
                .iter()
                .any(|prefix| line.starts_with(prefix)),
            "unexpected host line: {line}"
        );
    }
}

/// Pure extraction of escalation marks, intervals, host provenance, and the verdict-free
/// artifact schema from a parsed schema-v1 report. Task 4's real-process capture depends on
/// this module's public names verbatim.
mod extract {
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::process::Command;

    use mlx_guard_core::{
        CheckpointRecord, CheckpointStatus, SignalRecord, SignalResult, SignalTarget, TerminalKind,
        TerminalOutcome, checkpoint_signal_usr1,
    };
    use serde::Serialize;

    /// TERM's fixed signal number in schema v1 (`docs/POLICY.md`); mlx-guard always sends it
    /// to the owned process group, never the cooperative endpoint.
    const TERM_SIGNAL: u8 = 15;
    /// KILL's fixed signal number in schema v1; always sent to the owned process group.
    const KILL_SIGNAL: u8 = 9;

    /// Escalation marks extracted from one supervised run's signal, checkpoint, and outcome
    /// records.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    // The shared `_at_ms` suffix is the frozen contract name Task 4 depends on verbatim, not an
    // accidental repetition to fix.
    #[allow(clippy::struct_field_names)]
    pub struct EscalationMarks {
        pub checkpoint_requested_at_ms: Option<u64>,
        pub checkpoint_ack_at_ms: Option<u64>,
        pub term_at_ms: Option<u64>,
        pub kill_at_ms: Option<u64>,
        pub quiet_at_ms: Option<u64>,
    }

    /// The first delivered record matching `number` and `target`, or `None`. Undelivered
    /// attempts (`ProcessMissing`, `PermissionDenied`, `Failed`) never count, and retries after
    /// the first delivered attempt are ignored — the mark is when the signal first landed.
    fn first_delivered_at(
        signals: &[SignalRecord],
        number: u8,
        target: SignalTarget,
    ) -> Option<u64> {
        signals
            .iter()
            .find(|record| {
                record.signal == number
                    && record.target == target
                    && record.result == SignalResult::Delivered
            })
            .map(|record| record.at_ms)
    }

    /// Whether a terminal kind counts as a "quiet" moment for envelope measurement. Excludes
    /// only the kinds where the supervisor itself failed to finish its own job (a supervisor
    /// fault, a lost report, or a launch/configuration failure that never reached supervision) —
    /// `PolicyIntervention`, `ChildExited`, and `ChildSignaled` all count, since every measured
    /// scenario in this instrument ends in a policy intervention (exit 75).
    fn is_quiet_kind(kind: &TerminalKind) -> bool {
        !matches!(
            kind,
            TerminalKind::SupervisorFailure
                | TerminalKind::PartialArtifactFailure
                | TerminalKind::LaunchNotFound
                | TerminalKind::LaunchNotExecutable
                | TerminalKind::InvalidConfiguration
        )
    }

    /// Extract the escalation marks from one report's signal, checkpoint, and outcome
    /// components (not the raw journal — schema v1 is the frozen contract this reads).
    #[must_use]
    pub fn extract_escalation_marks(
        signals: &[SignalRecord],
        checkpoint: Option<&CheckpointRecord>,
        outcome: &TerminalOutcome,
    ) -> EscalationMarks {
        let checkpoint_ack_at_ms = checkpoint.and_then(|record| {
            if record.status == CheckpointStatus::AcknowledgedUnverifiedDurability {
                record.at_ms
            } else {
                None
            }
        });
        EscalationMarks {
            checkpoint_requested_at_ms: first_delivered_at(
                signals,
                checkpoint_signal_usr1().get(),
                SignalTarget::CooperativeEndpoint,
            ),
            checkpoint_ack_at_ms,
            term_at_ms: first_delivered_at(signals, TERM_SIGNAL, SignalTarget::OwnedProcessGroup),
            kill_at_ms: first_delivered_at(signals, KILL_SIGNAL, SignalTarget::OwnedProcessGroup),
            quiet_at_ms: is_quiet_kind(&outcome.kind).then_some(outcome.at_ms),
        }
    }

    /// The intervals between one run's escalation marks, in milliseconds.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    // The shared `_ms` suffix is the frozen contract name Task 4 depends on verbatim, not an
    // accidental repetition to fix.
    #[allow(clippy::struct_field_names)]
    pub struct EnvelopeIntervals {
        pub request_to_ack_ms: Option<u64>,
        pub term_to_quiet_ms: Option<u64>,
        pub kill_to_quiet_ms: Option<u64>,
    }

    /// The gap from `earlier` to `later`, or `None` if either mark is missing or the marks are
    /// out of the expected order. `checked_sub` on unsigned milliseconds means an inverted pair
    /// (clock skew, a mislabeled mark) yields `None`, never a wrapped huge number.
    fn checked_gap(later: Option<u64>, earlier: Option<u64>) -> Option<u64> {
        later?.checked_sub(earlier?)
    }

    /// Compute the escalation intervals implied by one run's marks.
    #[must_use]
    pub fn intervals(marks: &EscalationMarks) -> EnvelopeIntervals {
        EnvelopeIntervals {
            request_to_ack_ms: checked_gap(
                marks.checkpoint_ack_at_ms,
                marks.checkpoint_requested_at_ms,
            ),
            term_to_quiet_ms: checked_gap(marks.quiet_at_ms, marks.term_at_ms),
            kill_to_quiet_ms: checked_gap(marks.quiet_at_ms, marks.kill_at_ms),
        }
    }

    /// Parse `sysctl -n vm.loadavg`'s brace-delimited three-figure shape, e.g.
    /// `"{ 1.78 2.01 2.05 }"`. Returns `None` for anything that does not carry exactly three
    /// parseable figures.
    #[must_use]
    pub fn parse_loadavg(line: &str) -> Option<[f64; 3]> {
        let trimmed = line
            .trim()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .trim();
        let mut figures = trimmed.split_whitespace();
        let one: f64 = figures.next()?.parse().ok()?;
        let two: f64 = figures.next()?.parse().ok()?;
        let three: f64 = figures.next()?.parse().ok()?;
        if figures.next().is_some() {
            return None;
        }
        Some([one, two, three])
    }

    /// Host, toolchain, and repository provenance captured for one escalation-envelope run.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub struct EnvelopeProvenance {
        pub profile: String,
        pub host: Vec<String>,
        pub cpu_count: u64,
        pub memory_bytes: u64,
        pub load_at_start: [f64; 3],
        pub git_commit: String,
        pub git_status_porcelain: String,
        pub captured_at_utc: String,
        pub resolution_ms: u64,
    }

    /// The same five-field allowlist `reference_calibration.rs` uses to keep serial numbers,
    /// UUIDs, and UDIDs out of a committed evidence artifact.
    const ALLOWED_HOST_PREFIXES: [&str; 5] = [
        "Model Name:",
        "Model Identifier:",
        "Chip:",
        "Total Number of Cores:",
        "Memory:",
    ];

    fn sanitized_host_lines(raw: &str) -> Vec<String> {
        raw.lines()
            .map(str::trim)
            .filter(|line| {
                ALLOWED_HOST_PREFIXES
                    .iter()
                    .any(|prefix| line.starts_with(prefix))
            })
            .map(str::to_owned)
            .collect()
    }

    fn command_output(program: &str, args: &[&str]) -> String {
        let output = Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("failed to run {program} {args:?}: {error}"));
        assert!(
            output.status.success(),
            "{program} {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap_or_else(|error| panic!("{program} {args:?} produced non-UTF-8 output: {error}"))
            .trim()
            .to_owned()
    }

    /// Shell out to collect this host's provenance for an escalation-envelope capture.
    ///
    /// # Panics
    ///
    /// Panics if `system_profiler`, `sysctl`, `git`, or `date` cannot be run, exits non-zero, or
    /// produces output this function cannot parse (non-UTF-8 text, a malformed `vm.loadavg`
    /// line, or a non-numeric core count / memory size). This is test-only instrumentation, not
    /// supervision-path code, so a hard failure here is the correct behavior.
    #[must_use]
    pub fn capture_provenance(profile: &str, resolution_ms: u64) -> EnvelopeProvenance {
        let raw_hardware = command_output("system_profiler", &["SPHardwareDataType"]);
        let cpu_count = command_output("sysctl", &["-n", "hw.ncpu"])
            .parse()
            .unwrap_or_else(|error| panic!("hw.ncpu was not a valid integer: {error}"));
        let memory_bytes = command_output("sysctl", &["-n", "hw.memsize"])
            .parse()
            .unwrap_or_else(|error| panic!("hw.memsize was not a valid integer: {error}"));
        let loadavg_line = command_output("sysctl", &["-n", "vm.loadavg"]);
        let load_at_start = parse_loadavg(&loadavg_line)
            .unwrap_or_else(|| panic!("vm.loadavg did not parse: {loadavg_line:?}"));
        EnvelopeProvenance {
            profile: profile.to_owned(),
            host: sanitized_host_lines(&raw_hardware),
            cpu_count,
            memory_bytes,
            load_at_start,
            git_commit: command_output("git", &["rev-parse", "HEAD"]),
            git_status_porcelain: command_output("git", &["status", "--porcelain"]),
            captured_at_utc: command_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
            resolution_ms,
        }
    }

    /// One interval's raw per-repetition samples plus its p95 and maximum, summarized only over
    /// the present (`Some`) values.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub struct IntervalSummary {
        pub raw: Vec<Option<u64>>,
        pub p95_ms: Option<u64>,
        pub maximum_ms: Option<u64>,
        pub missing: u64,
    }

    impl IntervalSummary {
        /// Summarize `raw` into its p95 and maximum over the present values, following the same
        /// `(n * 95).div_ceil(100) - 1` sorted-index convention `intervention_process.rs` uses.
        /// An entirely-missing set (`raw` all `None`) summarizes to `p95_ms`/`maximum_ms` of
        /// `None` rather than indexing an empty vector.
        #[must_use]
        pub fn from_raw(raw: Vec<Option<u64>>) -> Self {
            let missing = u64::try_from(raw.iter().filter(|value| value.is_none()).count())
                .unwrap_or(u64::MAX);
            let mut present: Vec<u64> = raw.iter().filter_map(|value| *value).collect();
            present.sort_unstable();
            let (p95_ms, maximum_ms) = if present.is_empty() {
                (None, None)
            } else {
                let index = (present.len() * 95).div_ceil(100) - 1;
                (Some(present[index]), present.last().copied())
            };
            Self {
                raw,
                p95_ms,
                maximum_ms,
                missing,
            }
        }
    }

    /// One scenario's aggregate escalation-envelope measurements across its repetitions.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub struct ScenarioSummary {
        pub name: String,
        pub repetitions: u64,
        pub request_to_ack: Option<IntervalSummary>,
        pub term_to_quiet: Option<IntervalSummary>,
        pub kill_to_quiet: Option<IntervalSummary>,
        pub timed_out: u64,
        pub escalated_to_kill: u64,
        pub load_at_start_per_repetition: Vec<[f64; 3]>,
    }

    /// Serialize the verdict-free escalation-envelope artifact: `schema_version`, `unit`, the
    /// flattened host/toolchain provenance, and the per-scenario summaries. Never carries a
    /// `passed` or `target` field — this instrument records numbers, it does not grade them.
    ///
    /// # Errors
    ///
    /// Returns an error if the JSON cannot be serialized (unexpected for these plain-data types)
    /// or `path` cannot be written.
    pub fn write_envelope_artifact(
        path: &Path,
        provenance: &EnvelopeProvenance,
        scenarios: &[ScenarioSummary],
    ) -> io::Result<()> {
        #[derive(Serialize)]
        struct Artifact<'a> {
            schema_version: u32,
            unit: &'static str,
            #[serde(flatten)]
            provenance: &'a EnvelopeProvenance,
            scenarios: &'a [ScenarioSummary],
        }

        let artifact = Artifact {
            schema_version: 1,
            unit: "milliseconds",
            provenance,
            scenarios,
        };
        let bytes = serde_json::to_vec_pretty(&artifact)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::write(path, bytes)
    }
}
