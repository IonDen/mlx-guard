#![cfg(target_os = "macos")]
//! Pure core of the escalation-envelope measurement instrument.
//!
//! `mod extract` turns a parsed schema-v1 report's signal, checkpoint, and outcome records into
//! typed escalation marks and the intervals between them, captures host provenance for a
//! measurement run, and serializes a verdict-free JSON artifact. The unit tests exercise that
//! pure core; `capture_escalation_envelope` drives the shipped supervisor binary through four
//! real escalation scenarios and feeds their reports into the same module.
//!
//! Nothing in this file asserts a latency target. The capture asserts only that each scenario
//! really produced the escalation marks its published numbers are derived from, so a run that
//! stopped escalating fails loudly instead of publishing an empty envelope.

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use mlx_guard_core::{
    CheckpointRecord, CheckpointStatus, ChildStatus, Observed, ReportV1, SignalRecord,
    SignalResult, SignalTarget, TerminalKind, TerminalOutcome, checkpoint_signal_usr1,
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

fn unique_scratch_directory(tag: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "mlx-guard-envelope-{tag}-{}-{timestamp}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    // The supervisor refuses to initialize its journal anywhere that is not owner-only, so every
    // scratch directory the capture hands it has to be 0700 before the run starts.
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
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

// ---------------------------------------------------------------------------------------------
// Real-process capture. Everything below drives the shipped supervisor binary through the four
// escalation scenarios and records what the resulting schema-v1 reports contain.
// ---------------------------------------------------------------------------------------------

/// The shipped supervisor binary. Published evidence measures the real process boundary, not an
/// in-process `execute` call.
const GUARD: &str = env!("CARGO_BIN_EXE_mlx-guard");

/// Repetitions per scenario. Twenty matches `reference_runtime_calibration.rs`, which makes the
/// `(n * 95).div_ceil(100) - 1` index the second-largest sample.
const REPETITIONS: usize = 20;

/// The sampling interval every scenario runs at. The supervisor stamps its marks from the
/// sampling loop, so this is also the quantization of every published interval.
const SAMPLE_INTERVAL: &str = "10ms";

/// `SAMPLE_INTERVAL` as the number the artifact's provenance publishes.
const RESOLUTION_MS: u64 = 10;

/// The repetition count as the artifact's typed field.
fn repetition_count() -> u64 {
    u64::try_from(REPETITIONS).expect("repetition count fits u64")
}

/// The fixture binary the script builds and exports, matching
/// `reference_runtime_calibration.rs`'s pattern: the capture never guesses a target path.
fn fixture_path() -> PathBuf {
    std::env::var_os("MLX_GUARD_FIXTURE")
        .map(PathBuf::from)
        .expect("MLX_GUARD_FIXTURE must name the built fixture binary")
        .canonicalize()
        .expect("MLX_GUARD_FIXTURE must resolve to a built fixture binary")
}

/// This host's one-, five-, and fifteen-minute load averages, read fresh for every repetition so
/// the artifact discloses what else the machine was doing under each measured run.
fn load_average() -> [f64; 3] {
    let output = Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .expect("sysctl -n vm.loadavg must run");
    assert!(
        output.status.success(),
        "sysctl -n vm.loadavg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let line = String::from_utf8(output.stdout).expect("vm.loadavg must be UTF-8");
    parse_loadavg(&line).unwrap_or_else(|| panic!("vm.loadavg did not parse: {line:?}"))
}

/// Read one repetition's final report with the same strict parse the runtime tests use: schema,
/// privacy, ordering, and cross-field validation all have to pass before a number is recorded.
fn read_report(path: &Path) -> ReportV1 {
    ReportV1::from_json(&fs::read_to_string(path).unwrap()).unwrap()
}

/// Run the checkpoint scenarios' supervised command. The wall-time deadline is the trigger; the
/// fixture's own 9 s ceiling sits far beyond the worst-case escalation so a fixture self-exit can
/// never masquerade as a supervised termination.
fn supervise_checkpoint(fixture: &Path, report_path: &Path) -> Output {
    Command::new(GUARD)
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "1s",
            "--checkpoint-timeout",
            "5s",
            "--sample-interval",
            SAMPLE_INTERVAL,
            "--report",
        ])
        .arg(report_path)
        .arg("--")
        .arg(fixture)
        .args(["checkpoint-success", "1", "9000"])
        .stdin(Stdio::null())
        .output()
        .expect("the supervisor binary must run")
}

/// Run a group scenario's supervised command: a sixteen-member fanout whose own 5 s ceiling
/// cannot fire before the supervisor's 1500 ms wall deadline drives the escalation.
fn supervise_fanout(fixture: &Path, report_path: &Path, mode: &str) -> Output {
    Command::new(GUARD)
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "1500ms",
            "--sample-interval",
            SAMPLE_INTERVAL,
            "--report",
        ])
        .arg(report_path)
        .arg("--")
        .arg(fixture)
        .args([mode, "16", "5000"])
        .stdin(Stdio::null())
        .output()
        .expect("the supervisor binary must run")
}

/// What else the harness is running while a repetition is measured.
#[derive(Clone, Copy)]
enum BackgroundLoad {
    /// Nothing the harness controls.
    Idle,
    /// A fresh unsupervised sixteen-member stall group per repetition.
    FanoutStall,
}

/// Spawn the unsupervised load group and return only once its root has announced that every
/// member exists. Without reading that line the "loaded" arm could measure an idle machine.
fn spawn_load_group(fixture: &Path) -> Child {
    let mut child = Command::new(fixture)
        .args(["fanout-stall", "16", "4000"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the load group must spawn");
    let stdout = child
        .stdout
        .take()
        .expect("the load group's stdout must be piped");
    let ready = BufReader::new(stdout)
        .lines()
        .next()
        .expect("the load group ended before announcing readiness")
        .expect("the load group's readiness line must be readable");
    assert_eq!(ready, "READY mode=fanout-stall members=16");
    child
}

/// Wait for a load group to expire on its own. Nothing is signalled: every member self-terminates
/// at the 4 s wall its argv set, so a slow repetition can outlive its load and a decaying member
/// can overlap the next one. Both directions are disclosed with the artifact.
fn wait_for_load_group(mut root: Child) {
    root.wait().expect("the load group root must be waitable");
}

/// Capture one of the checkpoint-acknowledgement scenarios (A1 idle, A2 loaded).
///
/// A repetition whose acknowledgement times out records no `request_to_ack` interval and is
/// counted instead; only a scenario in which every repetition timed out is a failure, since an
/// acknowledgement window is a negotiation with a real worker, not a bound this test may enforce.
fn capture_checkpoint_scenario(
    name: &str,
    fixture: &Path,
    background: BackgroundLoad,
) -> ScenarioSummary {
    let mut request_to_ack = Vec::with_capacity(REPETITIONS);
    let mut term_to_quiet = Vec::with_capacity(REPETITIONS);
    let mut load_at_start_per_repetition = Vec::with_capacity(REPETITIONS);
    let mut timed_out = 0_u64;
    for repetition in 0..REPETITIONS {
        let directory = unique_scratch_directory(&format!("{name}-{repetition}"));
        let report_path = directory.join("report.json");
        let load_group = match background {
            BackgroundLoad::Idle => None,
            BackgroundLoad::FanoutStall => Some(spawn_load_group(fixture)),
        };
        // Read the load average last, immediately before the measured run starts.
        let load_at_start = load_average();

        let output = supervise_checkpoint(fixture, &report_path);

        if let Some(root) = load_group {
            wait_for_load_group(root);
        }
        assert_eq!(output.status.code(), Some(75), "{output:?}");
        let report = read_report(&report_path);
        let marks =
            extract_escalation_marks(&report.signals, Some(&report.checkpoint), &report.outcome);
        assert!(marks.checkpoint_requested_at_ms.is_some(), "{report:#?}");
        assert!(marks.term_at_ms.is_some(), "{report:#?}");
        assert!(marks.quiet_at_ms.is_some(), "{report:#?}");
        assert_eq!(
            report.outcome.child_status,
            Some(ChildStatus::Signaled { signal: 15 }),
            "{report:#?}"
        );
        if report.checkpoint.status == CheckpointStatus::TimedOut {
            timed_out += 1;
        }
        let computed = intervals(&marks);
        request_to_ack.push(computed.request_to_ack_ms);
        term_to_quiet.push(computed.term_to_quiet_ms);
        load_at_start_per_repetition.push(load_at_start);
        fs::remove_dir_all(&directory).unwrap();
    }
    assert!(
        timed_out < repetition_count(),
        "every {name} repetition timed out waiting for an acknowledgement"
    );
    // A published interval that no repetition produced is an empty envelope, not a measurement.
    // The per-repetition rule above forgives an acknowledgement that timed out, but a checkpoint
    // status the extraction does not recognize as acknowledged — one left at `RequestedUnverified`
    // or turned into `Cancelled` — would leave every value missing while `timed_out` stayed zero
    // and every other assertion passed. These floors are what makes that regression loud. They
    // bound observation, never latency.
    assert!(
        request_to_ack.iter().any(Option::is_some),
        "no {name} repetition produced an acknowledgement interval"
    );
    assert!(
        term_to_quiet.iter().any(Option::is_some),
        "no {name} repetition produced a term-to-quiet interval"
    );
    ScenarioSummary {
        name: name.to_owned(),
        repetitions: repetition_count(),
        request_to_ack: Some(IntervalSummary::from_raw(request_to_ack)),
        term_to_quiet: Some(IntervalSummary::from_raw(term_to_quiet)),
        kill_to_quiet: None,
        timed_out,
        escalated_to_kill: 0,
        load_at_start_per_repetition,
    }
}

/// Capture scenario B: a sixteen-member group with the default TERM disposition, which the first
/// escalation step is enough to end.
fn capture_group_term(name: &str, fixture: &Path) -> ScenarioSummary {
    let mut term_to_quiet = Vec::with_capacity(REPETITIONS);
    let mut load_at_start_per_repetition = Vec::with_capacity(REPETITIONS);
    for repetition in 0..REPETITIONS {
        let directory = unique_scratch_directory(&format!("{name}-{repetition}"));
        let report_path = directory.join("report.json");
        let load_at_start = load_average();

        let output = supervise_fanout(fixture, &report_path, "fanout-stall");

        assert_eq!(output.status.code(), Some(75), "{output:?}");
        let report = read_report(&report_path);
        let marks =
            extract_escalation_marks(&report.signals, Some(&report.checkpoint), &report.outcome);
        assert!(marks.term_at_ms.is_some(), "{report:#?}");
        assert!(marks.quiet_at_ms.is_some(), "{report:#?}");
        // Signalled-15 is what separates a supervised termination from the fixture's own
        // watchdog, which would show up as an exit code instead.
        assert_eq!(
            report.outcome.child_status,
            Some(ChildStatus::Signaled { signal: 15 }),
            "{report:#?}"
        );
        term_to_quiet.push(intervals(&marks).term_to_quiet_ms);
        load_at_start_per_repetition.push(load_at_start);
        fs::remove_dir_all(&directory).unwrap();
    }
    // An inverted mark pair turns every value missing without failing any per-repetition
    // assertion, so the published interval needs its own floor. Observation, not latency.
    assert!(
        term_to_quiet.iter().any(Option::is_some),
        "no {name} repetition produced a term-to-quiet interval"
    );
    ScenarioSummary {
        name: name.to_owned(),
        repetitions: repetition_count(),
        request_to_ack: None,
        term_to_quiet: Some(IntervalSummary::from_raw(term_to_quiet)),
        kill_to_quiet: None,
        timed_out: 0,
        escalated_to_kill: 0,
        load_at_start_per_repetition,
    }
}

/// Capture scenario C: a sixteen-member group that is deaf to TERM, so the escalation has to run
/// all the way to KILL.
///
/// No `term_to_quiet` is published here. `killpg` succeeds against a TERM-deaf group, so the TERM
/// mark is present but says nothing about when the group went quiet — the KILL is what ended it.
fn capture_group_kill(name: &str, fixture: &Path) -> ScenarioSummary {
    let mut kill_to_quiet = Vec::with_capacity(REPETITIONS);
    let mut load_at_start_per_repetition = Vec::with_capacity(REPETITIONS);
    let mut escalated_to_kill = 0_u64;
    for repetition in 0..REPETITIONS {
        let directory = unique_scratch_directory(&format!("{name}-{repetition}"));
        let report_path = directory.join("report.json");
        let load_at_start = load_average();

        let output = supervise_fanout(fixture, &report_path, "fanout-ignore-term");

        assert_eq!(output.status.code(), Some(75), "{output:?}");
        let report = read_report(&report_path);
        let marks =
            extract_escalation_marks(&report.signals, Some(&report.checkpoint), &report.outcome);
        assert!(marks.term_at_ms.is_some(), "{report:#?}");
        assert!(marks.kill_at_ms.is_some(), "{report:#?}");
        assert!(marks.quiet_at_ms.is_some(), "{report:#?}");
        assert_eq!(
            report.outcome.child_status,
            Some(ChildStatus::Signaled { signal: 9 }),
            "{report:#?}"
        );
        // Counted only after the KILL mark was asserted present, so the published count is the
        // number of repetitions that really escalated, not the number that were expected to.
        escalated_to_kill += 1;
        kill_to_quiet.push(intervals(&marks).kill_to_quiet_ms);
        load_at_start_per_repetition.push(load_at_start);
        fs::remove_dir_all(&directory).unwrap();
    }
    // An inverted mark pair turns every value missing without failing any per-repetition
    // assertion, so the published interval needs its own floor. Observation, not latency.
    assert!(
        kill_to_quiet.iter().any(Option::is_some),
        "no {name} repetition produced a kill-to-quiet interval"
    );
    ScenarioSummary {
        name: name.to_owned(),
        repetitions: repetition_count(),
        request_to_ack: None,
        term_to_quiet: None,
        kill_to_quiet: Some(IntervalSummary::from_raw(kill_to_quiet)),
        timed_out: 0,
        escalated_to_kill,
        load_at_start_per_repetition,
    }
}

#[test]
#[ignore = "measurement capture; run via scripts/measure-escalation-envelope.sh"]
fn capture_escalation_envelope() {
    // Catches an escalation path that stops producing the marks the published envelope is derived
    // from: each scenario asserts its promised marks, its exit code, and how the root really died
    // before any interval is recorded, so a silently-degraded run fails instead of publishing an
    // empty envelope. Nothing here bounds a latency.
    let output_path = PathBuf::from(
        std::env::var_os("MLX_GUARD_ENVELOPE_OUTPUT")
            .expect("MLX_GUARD_ENVELOPE_OUTPUT must name the artifact file path"),
    );
    let profile = std::env::var("MLX_GUARD_ENVELOPE_PROFILE")
        .expect("MLX_GUARD_ENVELOPE_PROFILE must label the capture environment");
    let fixture = fixture_path();

    let provenance = capture_provenance(&profile, RESOLUTION_MS);
    let scenarios = [
        capture_checkpoint_scenario("checkpoint_ack_idle", &fixture, BackgroundLoad::Idle),
        capture_checkpoint_scenario(
            "checkpoint_ack_loaded",
            &fixture,
            BackgroundLoad::FanoutStall,
        ),
        capture_group_term("group_term", &fixture),
        capture_group_kill("group_kill", &fixture),
    ];

    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).unwrap();
    }
    write_envelope_artifact(&output_path, &provenance, &scenarios).unwrap();

    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&output_path).unwrap()).unwrap();
    assert_eq!(
        value["scenarios"]
            .as_array()
            .expect("the artifact must carry a scenarios array")
            .len(),
        4
    );
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
