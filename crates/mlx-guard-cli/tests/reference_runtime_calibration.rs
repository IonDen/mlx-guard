#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_core::{
    CheckpointStatus, ChildStatus, JournalEntry, JournalRecovery, Observed, PolicyState, ReportV1,
    SignalTarget, TerminalKind,
};
use serde::Serialize;

const GUARD: &str = env!("CARGO_BIN_EXE_mlx-guard");
const RAMP_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const RAMP_REPETITIONS: usize = 20;
const TERMINAL_REPETITIONS: usize = 20;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
struct RuntimeCalibration {
    schema_version: u16,
    command: String,
    ramp: RampMeasurements,
    external_terminal: Vec<TerminalMeasurements>,
    bounds: Vec<BoundResult>,
}

#[derive(Serialize)]
struct RampMeasurements {
    rate_bytes_per_second: u64,
    limit_bytes: u64,
    raw: Vec<RampSample>,
    overshoot_p95_bytes: u64,
    decision_to_signal_p95_milliseconds: u64,
    observed_decrease_count: usize,
    right_censored_decrease_count: usize,
}

#[derive(Serialize)]
struct RampSample {
    triggering_footprint_bytes: u64,
    overshoot_bytes: u64,
    decision_at_milliseconds: u64,
    first_group_signal_at_milliseconds: u64,
    decision_to_signal_milliseconds: u64,
    observed_decrease_latency_milliseconds: Option<u64>,
    finalization_latency_milliseconds: u64,
}

#[derive(Serialize)]
struct TerminalMeasurements {
    signal: &'static str,
    raw_finalization_nanoseconds: Vec<u64>,
    p95_finalization_nanoseconds: u64,
    maximum_finalization_nanoseconds: u64,
}

#[derive(Serialize)]
struct BoundResult {
    measure: &'static str,
    target: &'static str,
    observed: u64,
    unit: &'static str,
    passed: bool,
}

#[derive(Serialize)]
struct ScenarioEvidence {
    schema_version: u16,
    command: String,
    scenarios: Vec<ScenarioRecord>,
    safe_workloads: Vec<ScenarioRecord>,
    false_interventions: usize,
}

#[derive(Serialize)]
struct ScenarioRecord {
    name: String,
    exit_code: i32,
    report_file: Option<String>,
    outcome: String,
    checkpoint: Option<String>,
    signals: Vec<u8>,
    escape_detected: Option<bool>,
    sample_count: usize,
    diagnostic: Option<String>,
}

struct PrivateDirectory(PathBuf);

impl PrivateDirectory {
    fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-runtime-calibration-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn fixture_path() -> PathBuf {
    std::env::var_os("MLX_GUARD_FIXTURE")
        .map(PathBuf::from)
        .expect("MLX_GUARD_FIXTURE must name the built fixture binary")
        .canonicalize()
        .expect("MLX_GUARD_FIXTURE must resolve to a built fixture binary")
}

fn output_path() -> PathBuf {
    std::env::var_os("MLX_GUARD_RUNTIME_CALIBRATION_OUTPUT")
        .map(PathBuf::from)
        .expect("MLX_GUARD_RUNTIME_CALIBRATION_OUTPUT must name the JSON output path")
}

fn report(path: &Path) -> ReportV1 {
    ReportV1::from_json(&fs::read_to_string(path).unwrap()).unwrap()
}

fn owned_group_signals(report: &ReportV1) -> Vec<u8> {
    report
        .signals
        .iter()
        .filter(|signal| signal.target == SignalTarget::OwnedProcessGroup)
        .map(|signal| signal.signal)
        .collect()
}

fn observed(value: &Observed<u64>) -> Option<u64> {
    match value {
        Observed::Available { value } => Some(*value),
        Observed::Unknown
        | Observed::Unavailable { .. }
        | Observed::Stale { .. }
        | Observed::Error { .. } => None,
    }
}

fn percentile95(mut values: Vec<u64>) -> (Vec<u64>, u64, u64) {
    values.sort_unstable();
    let index = (values.len() * 95).div_ceil(100) - 1;
    let p95 = values[index];
    let maximum = *values.last().unwrap();
    (values, p95, maximum)
}

fn run_guard(report_path: &Path, mode_args: &[&str], worker_args: &[String]) -> Output {
    Command::new(GUARD)
        .args(mode_args)
        .arg("--report")
        .arg(report_path)
        .arg("--")
        .args(worker_args)
        .output()
        .unwrap()
}

/// The residual stderr worth recording as a scenario diagnostic, with the expected pre-launch
/// banner (`run` announces the emergency threshold before launching) filtered out so it never reads
/// as an anomaly in the calibration evidence.
fn scenario_diagnostic(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let residual = text
        .lines()
        .filter(|line| !line.starts_with("mlx-guard: enforcing a"))
        .collect::<Vec<_>>()
        .join("\n");
    let residual = residual.trim().to_owned();
    (!residual.is_empty()).then_some(residual)
}

#[test]
fn scenario_diagnostic_drops_the_launch_banner_but_keeps_a_real_anomaly() {
    // Catches the filter over-matching (swallowing a real anomaly line) or under-matching
    // (recording the expected launch banner as though it were a diagnostic).
    let banner = b"mlx-guard: enforcing a 100-byte footprint limit; emergency KILL at 110 bytes, about 10% above the limit\n";
    assert_eq!(scenario_diagnostic(banner), None);
    assert_eq!(
        scenario_diagnostic(b"mlx-guard: artifact read failed\n"),
        Some("mlx-guard: artifact read failed".to_owned())
    );
    let mixed = b"mlx-guard: enforcing a 100-byte footprint limit; emergency KILL at 110 bytes, about 10% above the limit\nmlx-guard: artifact read failed\n";
    assert_eq!(
        scenario_diagnostic(mixed),
        Some("mlx-guard: artifact read failed".to_owned())
    );
    assert_eq!(scenario_diagnostic(b""), None);
}

fn scenario_record(
    output_directory: &Path,
    name: &str,
    mode_args: &[&str],
    worker_args: &[String],
) -> (ScenarioRecord, ReportV1) {
    let relative_report = format!("reports/{name}.json");
    let report_path = output_directory.join(&relative_report);
    let output = run_guard(&report_path, mode_args, worker_args);
    let report = report(&report_path);
    let escape_detected = match report.escape.detected {
        Observed::Available { value } => Some(value),
        Observed::Unknown
        | Observed::Unavailable { .. }
        | Observed::Stale { .. }
        | Observed::Error { .. } => None,
    };
    let record = ScenarioRecord {
        name: name.to_owned(),
        exit_code: output.status.code().unwrap(),
        report_file: Some(relative_report),
        outcome: format!("{:?}", report.outcome.kind),
        checkpoint: Some(format!("{:?}", report.checkpoint.status)),
        signals: report.signals.iter().map(|signal| signal.signal).collect(),
        escape_detected,
        sample_count: report.samples.len(),
        diagnostic: scenario_diagnostic(&output.stderr),
    };
    (record, report)
}

fn fixture_worker(mode: &str, value: u64, wall_ms: u64) -> Vec<String> {
    vec![
        fixture_path().display().to_string(),
        mode.to_owned(),
        value.to_string(),
        wall_ms.to_string(),
    ]
}

fn prepare_scenario_output() -> PathBuf {
    let path = std::env::var_os("MLX_GUARD_SCENARIO_OUTPUT_DIRECTORY")
        .map(PathBuf::from)
        .expect("MLX_GUARD_SCENARIO_OUTPUT_DIRECTORY must name a new output directory");
    assert!(!path.exists(), "scenario output directory must not exist");
    fs::create_dir_all(path.join("reports")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(path.join("reports"), fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn late_storage_error(output_directory: &Path) -> ScenarioRecord {
    let report_path = output_directory.join("reports/storage-error.json");
    let journal_path = output_directory.join("reports/.storage-error.json.journal");
    let guard = Command::new(GUARD)
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "100ms",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args(["--", "/bin/sleep", "5"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !journal_path.exists() {
        assert!(
            Instant::now() < deadline,
            "journal initialization timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    fs::remove_file(journal_path).unwrap();
    let output = guard.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(74));
    assert!(!report_path.exists());
    ScenarioRecord {
        name: "storage_error".to_owned(),
        exit_code: 74,
        report_file: None,
        outcome: "PartialArtifactFailure".to_owned(),
        checkpoint: None,
        signals: vec![15],
        escape_detected: None,
        sample_count: 0,
        diagnostic: scenario_diagnostic(&output.stderr),
    }
}

fn ramp_measurements() -> RampMeasurements {
    let directory = PrivateDirectory::new();
    let fixture = fixture_path();
    let mut raw = Vec::with_capacity(RAMP_REPETITIONS);
    for repetition in 0..RAMP_REPETITIONS {
        let report_path = directory.0.join(format!("ramp-{repetition}.json"));
        let output = run_guard(
            &report_path,
            &[
                "run",
                "--max-footprint",
                "64MiB",
                "--sample-interval",
                "50ms",
            ],
            &[
                fixture.display().to_string(),
                "ramp".to_owned(),
                (128 * 1024 * 1024_u64).to_string(),
                "3000".to_owned(),
            ],
        );
        assert_eq!(output.status.code(), Some(75), "{output:?}");
        let report = report(&report_path);
        let signal = report
            .signals
            .iter()
            .find(|signal| signal.target == SignalTarget::OwnedProcessGroup)
            .unwrap();
        let decision = report
            .transitions
            .iter()
            .filter(|transition| transition.at_ms <= signal.at_ms)
            .filter(|transition| {
                matches!(
                    transition.to,
                    PolicyState::CheckpointRequested
                        | PolicyState::Terminating
                        | PolicyState::Emergency
                )
            })
            .filter_map(|transition| {
                transition
                    .aggregate_footprint_bytes
                    .map(|footprint| (transition.at_ms, footprint))
            })
            .next_back()
            .unwrap();
        let decrease = report.samples.iter().find_map(|sample| {
            (sample.captured_at_ms > signal.at_ms)
                .then(|| observed(&sample.aggregate_footprint_bytes))
                .flatten()
                .filter(|footprint| *footprint < decision.1)
                .map(|_| sample.captured_at_ms.saturating_sub(signal.at_ms))
        });
        raw.push(RampSample {
            triggering_footprint_bytes: decision.1,
            overshoot_bytes: decision.1.saturating_sub(RAMP_LIMIT_BYTES),
            decision_at_milliseconds: decision.0,
            first_group_signal_at_milliseconds: signal.at_ms,
            decision_to_signal_milliseconds: signal.at_ms.saturating_sub(decision.0),
            observed_decrease_latency_milliseconds: decrease,
            finalization_latency_milliseconds: report.outcome.at_ms.saturating_sub(signal.at_ms),
        });
    }
    let (_, overshoot_p95_bytes, _) =
        percentile95(raw.iter().map(|sample| sample.overshoot_bytes).collect());
    let (_, decision_to_signal_p95_milliseconds, _) = percentile95(
        raw.iter()
            .map(|sample| sample.decision_to_signal_milliseconds)
            .collect(),
    );
    let observed_decrease_count = raw
        .iter()
        .filter(|sample| sample.observed_decrease_latency_milliseconds.is_some())
        .count();
    RampMeasurements {
        rate_bytes_per_second: 128 * 1024 * 1024,
        limit_bytes: RAMP_LIMIT_BYTES,
        raw,
        overshoot_p95_bytes,
        decision_to_signal_p95_milliseconds,
        observed_decrease_count,
        right_censored_decrease_count: RAMP_REPETITIONS - observed_decrease_count,
    }
}

fn wait_for_first_sample(journal_path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if let Ok(recovery) = JournalRecovery::read(journal_path)
            && recovery
                .records
                .iter()
                .any(|record| matches!(record.entry, JournalEntry::Sample(_)))
        {
            return;
        }
        assert!(Instant::now() < deadline, "first sample timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn terminal_measurements(signal: i32, name: &'static str) -> TerminalMeasurements {
    let directory = PrivateDirectory::new();
    let fixture = fixture_path();
    let mut raw = Vec::with_capacity(TERMINAL_REPETITIONS);
    for repetition in 0..TERMINAL_REPETITIONS {
        let report_path = directory.0.join(format!("{name}-{repetition}.json"));
        let journal_path = directory
            .0
            .join(format!(".{name}-{repetition}.json.journal"));
        let mut guard = Command::new(GUARD)
            .args(["observe", "--sample-interval", "10ms", "--report"])
            .arg(&report_path)
            .arg("--")
            .arg(&fixture)
            .args(["cpu-stall", "1", "3000"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_for_first_sample(&journal_path);
        let started = Instant::now();
        // SAFETY: guard is a live child PID and signal is SIGTERM or SIGINT.
        assert_eq!(unsafe { libc::kill(guard.id().cast_signed(), signal) }, 0);
        let status = guard.wait().unwrap();
        raw.push(u64::try_from(started.elapsed().as_nanos()).unwrap());
        assert_eq!(status.code(), Some(128 + signal));
        assert!(report_path.is_file());
        let report = report(&report_path);
        assert!(report.signals.iter().any(|record| {
            i32::from(record.signal) == signal && record.target == SignalTarget::OwnedProcessGroup
        }));
    }
    let (raw, p95, maximum) = percentile95(raw);
    TerminalMeasurements {
        signal: name,
        raw_finalization_nanoseconds: raw,
        p95_finalization_nanoseconds: p95,
        maximum_finalization_nanoseconds: maximum,
    }
}

#[test]
#[ignore = "bounded M1 Max runtime calibration"]
fn reference_host_runtime_measurements_write_raw_and_derived_json() {
    // Catches publishing timing claims without per-run values that independently derive the p95.
    let ramp = ramp_measurements();
    let external_terminal = vec![
        terminal_measurements(libc::SIGTERM, "SIGTERM"),
        terminal_measurements(libc::SIGINT, "SIGINT"),
    ];
    let bounds = vec![
        BoundResult {
            measure: "128 MiB/s bounded-ramp threshold overshoot p95",
            target: "<= 16 MiB",
            observed: ramp.overshoot_p95_bytes,
            unit: "bytes",
            passed: ramp.overshoot_p95_bytes <= 16 * 1024 * 1024,
        },
        BoundResult {
            measure: "threshold decision to first signal p95",
            target: "<= 10 ms",
            observed: ramp.decision_to_signal_p95_milliseconds,
            unit: "milliseconds",
            passed: ramp.decision_to_signal_p95_milliseconds <= 10,
        },
        BoundResult {
            measure: "external TERM to final report p95",
            target: "<= 100 ms",
            observed: external_terminal[0].p95_finalization_nanoseconds,
            unit: "nanoseconds",
            passed: external_terminal[0].p95_finalization_nanoseconds <= 100_000_000,
        },
        BoundResult {
            measure: "external INT to final report p95",
            target: "<= 100 ms",
            observed: external_terminal[1].p95_finalization_nanoseconds,
            unit: "nanoseconds",
            passed: external_terminal[1].p95_finalization_nanoseconds <= 100_000_000,
        },
    ];
    let passed = bounds.iter().all(|bound| bound.passed);
    let result = RuntimeCalibration {
        schema_version: 1,
        command: "MLX_GUARD_FIXTURE=<path> MLX_GUARD_RUNTIME_CALIBRATION_OUTPUT=<path> cargo test -p mlx-guard-cli --test reference_runtime_calibration reference_host_runtime_measurements_write_raw_and_derived_json -- --ignored --exact --nocapture".to_owned(),
        ramp,
        external_terminal,
        bounds,
    };
    let path = output_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    assert!(
        passed,
        "one or more predeclared bounds missed; see {path:?}"
    );
}

#[test]
#[ignore = "bounded lifecycle scenario and safe-workload evidence"]
#[allow(clippy::too_many_lines)] // Keeping the evidence matrix linear makes omissions auditable.
fn reference_host_scenarios_write_reports_and_false_intervention_count() {
    // Catches claiming lifecycle coverage from unit-only state transitions without real reports.
    let output_directory = prepare_scenario_output();
    let mut scenarios = Vec::new();

    let (normal, report) = scenario_record(
        &output_directory,
        "normal-exit",
        &[
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
        ],
        &["/bin/sleep".to_owned(), "0.08".to_owned()],
    );
    assert_eq!(report.outcome.kind, TerminalKind::ChildExited { code: 0 });
    assert!(report.signals.is_empty());
    scenarios.push(normal);

    let (observe, report) = scenario_record(
        &output_directory,
        "observe-only",
        &["observe", "--sample-interval", "10ms"],
        &["/bin/sleep".to_owned(), "0.08".to_owned()],
    );
    assert_eq!(report.outcome.kind, TerminalKind::ChildExited { code: 0 });
    assert!(report.signals.is_empty());
    scenarios.push(observe);

    let (acknowledged, report) = scenario_record(
        &output_directory,
        "checkpoint-acknowledged",
        &[
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "1s",
            "--sample-interval",
            "10ms",
        ],
        &fixture_worker("checkpoint-success", 1, 3_000),
    );
    assert_eq!(
        report.checkpoint.status,
        CheckpointStatus::AcknowledgedUnverifiedDurability
    );
    assert!(report.signals.iter().any(|signal| signal.signal == 15));
    scenarios.push(acknowledged);

    let (timeout, report) = scenario_record(
        &output_directory,
        "checkpoint-timeout",
        &[
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "1s",
            "--sample-interval",
            "10ms",
            // The worker withholds its acknowledgement for 500 ms; the scenario records a deadline
            // it misses, so the timeout is pinned below that rather than left at the 1 s default.
            "--checkpoint-timeout",
            "100ms",
        ],
        &fixture_worker("checkpoint-blocked", 500, 3_000),
    );
    assert_eq!(report.checkpoint.status, CheckpointStatus::TimedOut);
    assert!(report.signals.iter().any(|signal| signal.signal == 15));
    scenarios.push(timeout);

    let (term, report) = scenario_record(
        &output_directory,
        "term",
        &[
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "30ms",
            "--sample-interval",
            "10ms",
        ],
        &["/bin/sleep".to_owned(), "1".to_owned()],
    );
    assert_eq!(owned_group_signals(&report), [15]);
    scenarios.push(term);

    let (kill, report) = scenario_record(
        &output_directory,
        "kill",
        &[
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "30ms",
            "--sample-interval",
            "10ms",
        ],
        &fixture_worker("ignore-term", 1, 2_000),
    );
    assert_eq!(owned_group_signals(&report), [15, 9]);
    scenarios.push(kill);

    let (fast_root, report) = scenario_record(
        &output_directory,
        "root-fast-exit",
        &[
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
        ],
        &fixture_worker("fast-root-exit", 1, 500),
    );
    assert_eq!(report.outcome.kind, TerminalKind::ChildExited { code: 23 });
    assert_eq!(owned_group_signals(&report), [15]);
    assert!(
        !report
            .transitions
            .iter()
            .any(|transition| transition.to == PolicyState::SupervisorError)
    );
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 23 })
    );
    scenarios.push(fast_root);

    let (group_cleanup, report) = scenario_record(
        &output_directory,
        "owned-group-cleanup",
        &[
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "30ms",
            "--sample-interval",
            "10ms",
        ],
        &fixture_worker("fanout-stall", 4, 2_000),
    );
    assert_eq!(report.outcome.kind, TerminalKind::PolicyIntervention);
    assert_eq!(owned_group_signals(&report), [15]);
    scenarios.push(group_cleanup);

    let (escape, report) = scenario_record(
        &output_directory,
        "session-escape",
        &[
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "300ms",
            "--sample-interval",
            "10ms",
        ],
        &fixture_worker("setsid-parent", 1, 800),
    );
    assert_eq!(report.escape.detected, Observed::Available { value: true });
    scenarios.push(escape);
    scenarios.push(late_storage_error(&output_directory));

    let safe_cases = [
        (
            "safe-observe-sleep",
            vec!["observe", "--sample-interval", "10ms"],
            vec!["/bin/sleep".to_owned(), "0.08".to_owned()],
        ),
        (
            "safe-run-sleep",
            vec![
                "run",
                "--max-footprint",
                "1TiB",
                "--sample-interval",
                "10ms",
            ],
            vec!["/bin/sleep".to_owned(), "0.08".to_owned()],
        ),
        (
            "safe-observe-cpu",
            vec!["observe", "--sample-interval", "10ms"],
            fixture_worker("cpu-stall", 1, 100),
        ),
        (
            "safe-run-cpu",
            vec![
                "run",
                "--max-footprint",
                "1TiB",
                "--sample-interval",
                "10ms",
            ],
            fixture_worker("cpu-stall", 1, 100),
        ),
    ];
    let mut safe_workloads = Vec::new();
    let mut false_interventions = 0;
    for (name, mode_args, worker) in safe_cases {
        let (record, report) = scenario_record(&output_directory, name, &mode_args, &worker);
        // Every safe workload is a single process with no owned-group members that could
        // survive a root exit, so no RootExitCleanup signal is possible here: any signal at
        // all is unexpected.
        let unexpected_signal = !report.signals.is_empty();
        if report.outcome.kind == TerminalKind::PolicyIntervention || unexpected_signal {
            false_interventions += 1;
        }
        assert_eq!(report.outcome.kind, TerminalKind::ChildExited { code: 0 });
        assert!(report.signals.is_empty());
        safe_workloads.push(record);
    }
    assert_eq!(false_interventions, 0);

    let evidence = ScenarioEvidence {
        schema_version: 1,
        command: "MLX_GUARD_FIXTURE=<path> MLX_GUARD_SCENARIO_OUTPUT_DIRECTORY=<new-directory> cargo test -p mlx-guard-cli --test reference_runtime_calibration reference_host_scenarios_write_reports_and_false_intervention_count -- --ignored --exact --nocapture".to_owned(),
        scenarios,
        safe_workloads,
        false_interventions,
    };
    fs::write(
        output_directory.join("scenarios.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
}
