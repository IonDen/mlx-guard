use std::process::Command;

#[cfg(target_os = "macos")]
use std::process::Stdio;

#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "macos")]
use std::thread;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "macos")]
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "macos")]
struct TestDirectory(PathBuf);

#[cfg(target_os = "macos")]
impl TestDirectory {
    fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-cli-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

#[cfg(target_os = "macos")]
impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[cfg(target_os = "macos")]
fn wait_for_first_sample(journal_path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if let Ok(recovery) = mlx_guard_core::JournalRecovery::read(journal_path)
            && recovery
                .records
                .iter()
                .any(|record| matches!(record.entry, mlx_guard_core::JournalEntry::Sample(_)))
        {
            return;
        }
        assert!(Instant::now() < deadline, "first sample timed out");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn help_is_successful_and_invalid_input_is_usage() {
    // Catches help being treated as an error or invalid input returning false success.
    let help = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .arg("--help")
        .output()
        .expect("the command must run");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: mlx-guard <COMMAND>"));
    assert!(help.stderr.is_empty());

    let invalid = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .output()
        .expect("the command must run");
    assert_eq!(invalid.status.code(), Some(64));
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("Usage: mlx-guard <COMMAND>"));
}

#[cfg(target_os = "macos")]
#[test]
fn unsafe_report_directory_fails_before_worker_launch() {
    // Catches launching the worker before mandatory secure journal initialization succeeds.
    let directory = TestDirectory::new();
    let marker = directory.0.join("worker-launched");
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "observe",
            "--report",
            "/tmp/mlx-guard-unsafe-report.json",
            "--",
            "/usr/bin/touch",
        ])
        .arg(&marker)
        .output()
        .expect("the command must run");
    assert_eq!(output.status.code(), Some(74));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "mlx-guard: artifact directory is not private and owned\n"
    );
    assert!(!marker.exists());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_platform_fails_before_worker_launch() {
    // Catches using a non-Darwin metric as if it were OS-accounted physical footprint.
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args(["observe", "--report", "/tmp/report.json", "--", "true"])
        .output()
        .expect("the command must run");
    assert_eq!(output.status.code(), Some(70));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "mlx-guard: OS-accounted footprint observation is unsupported on this platform\n"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn observe_supervises_a_real_child_and_writes_a_valid_report() {
    // Catches returning the child status without sampling, journalling, and finalizing the run.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");

    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args(["observe", "--sample-interval", "10ms", "--report"])
        .arg(&report_path)
        .args(["--", "/bin/sleep", "0.08"])
        .output()
        .expect("the command must run");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let summary = String::from_utf8(output.stdout).unwrap();
    assert!(summary.starts_with("mlx-guard: child_exited at "));
    assert!(summary.ends_with(" samples, 0 signals\n"));
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(&report_path).expect("final report must exist"),
    )
    .unwrap();
    assert_eq!(
        report.configuration.mode,
        mlx_guard_core::ReportMode::Observe
    );
    assert!(!report.samples.is_empty());
    assert!(report.transitions.is_empty());
    assert!(report.signals.is_empty());
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::ChildExited { code: 0 }
    );
    assert_eq!(
        fs::metadata(report_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(target_os = "macos")]
#[test]
fn run_wall_limit_drives_policy_term_and_returns_intervention_status() {
    // Catches waiting for normal child exit instead of executing the policy's wall-time action.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");

    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "30ms",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args(["--", "/bin/sleep", "1"])
        .output()
        .expect("the command must run");

    assert_eq!(
        output.status.code(),
        Some(75),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("mlx-guard: policy_intervention"));
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(report_path).expect("final report must exist"),
    )
    .unwrap();
    assert_eq!(
        report.configuration.mode,
        mlx_guard_core::ReportMode::Enforce
    );
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::PolicyIntervention
    );
    assert!(
        report.signals.iter().any(|signal| signal.signal == 15
            && signal.result == mlx_guard_core::SignalResult::Delivered)
    );
    assert!(
        report
            .transitions
            .iter()
            .any(|transition| transition.to == mlx_guard_core::PolicyState::Terminating)
    );
    assert_eq!(
        report.checkpoint.status,
        mlx_guard_core::CheckpointStatus::NotNegotiated
    );
}

#[cfg(target_os = "macos")]
#[test]
fn run_preserves_a_fast_child_exit_and_finalizes_its_report() {
    // Catches treating an exit-before-first-sample race as a supervisor identity failure.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");

    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args(["--", "/usr/bin/true"])
        .output()
        .expect("the command must run");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(report_path).expect("final report must exist"),
    )
    .unwrap();
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::ChildExited { code: 0 }
    );
    assert!(report.signals.is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn runtime_artifacts_and_guard_output_exclude_sensitive_launch_data() {
    // Catches orchestration bypassing the redacted report types or echoing untrusted argv bytes.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let argv_secret = "argv-secret-7f30b1";
    let env_secret = "env-secret-38ac92";
    let control_sequence = "\u{1b}[31muntrusted-control\u{1b}[0m";

    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
            "--clear-env",
            "--env",
        ])
        .arg(format!("TOKEN={env_secret}"))
        .args(["--cwd"])
        .arg(&directory.0)
        .args(["--report"])
        .arg(&report_path)
        .args(["--", "/usr/bin/true", argv_secret, control_sequence])
        .output()
        .expect("the command must run");

    assert_eq!(output.status.code(), Some(0));
    let report = fs::read_to_string(report_path).unwrap();
    let public_bytes = format!(
        "{}{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        report
    );
    for sensitive in [
        argv_secret,
        env_secret,
        control_sequence,
        directory.0.to_str().unwrap(),
        "/usr/bin/true",
    ] {
        assert!(!public_bytes.contains(sensitive), "leaked {sensitive:?}");
    }
    assert!(!public_bytes.contains('\u{1b}'));
}

#[cfg(target_os = "macos")]
#[test]
fn raw_child_output_is_not_control_data_or_a_persisted_guard_artifact() {
    // Catches parsing inherited worker output as control data or copying it into guard artifacts.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let child_canary = "CHILD_OUTPUT_SECRET_CANARY_5e91";
    let child_output = format!("\u{1b}[31m{child_canary}\u{1b}[0m");

    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args(["--", "/usr/bin/printf", "%s", &child_output])
        .output()
        .expect("the command must run");

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(output.stdout.starts_with(child_output.as_bytes()));
    for entry in fs::read_dir(&directory.0).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(child_canary),
            "guard artifact persisted inherited child output"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn run_emergency_footprint_breach_kills_the_owned_group() {
    // Catches applying the explicit sampled threshold only to wall time or graceful TERM.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");

    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "2B",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args(["--", "/bin/sleep", "1"])
        .output()
        .expect("the command must run");

    assert_eq!(
        output.status.code(),
        Some(75),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(report_path).expect("final report must exist"),
    )
    .unwrap();
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::PolicyIntervention
    );
    assert!(report.signals.iter().any(
        |signal| signal.signal == 9 && signal.result == mlx_guard_core::SignalResult::Delivered
    ));
    assert!(report.transitions.iter().any(|transition| {
        transition.from == mlx_guard_core::PolicyState::Normal
            && transition.to == mlx_guard_core::PolicyState::Emergency
    }));
}

#[cfg(target_os = "macos")]
#[test]
fn first_sigint_is_forwarded_and_the_child_signal_status_is_preserved() {
    // Catches the supervisor dying from SIGINT instead of forwarding and finalizing evidence.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let journal_path = directory.0.join(".report.json.journal");
    let guard = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args(["--", "/bin/sleep", "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the command must run");
    wait_for_first_sample(&journal_path);

    let delivered = Command::new("/bin/kill")
        .args(["-INT", &guard.id().to_string()])
        .status()
        .expect("SIGINT delivery command must run");
    assert!(delivered.success());
    let output = guard.wait_with_output().expect("supervisor must terminate");

    assert_eq!(output.status.code(), Some(130));
    assert!(output.stderr.is_empty());
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(report_path).expect("final report must exist"),
    )
    .unwrap();
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::ChildSignaled { signal: 2 }
    );
    assert!(report.signals.iter().any(|signal| {
        signal.signal == 2 && signal.result == mlx_guard_core::SignalResult::Delivered
    }));
}

#[cfg(target_os = "macos")]
#[test]
fn repeated_terminal_signal_escalates_to_group_kill() {
    // Catches a second terminal signal being forwarded or ignored instead of forcing KILL.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let journal_path = directory.0.join(".report.json.journal");
    let guard = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args([
            "--",
            "/bin/sh",
            "-c",
            "trap '' INT TERM; while :; do sleep 1; done",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the command must run");
    wait_for_first_sample(&journal_path);

    for signal in ["-TERM", "-INT"] {
        let delivered = Command::new("/bin/kill")
            .args([signal, &guard.id().to_string()])
            .status()
            .expect("terminal signal delivery command must run");
        assert!(delivered.success());
        thread::sleep(Duration::from_millis(20));
    }
    let output = guard.wait_with_output().expect("supervisor must terminate");

    assert_eq!(output.status.code(), Some(137));
    assert!(output.stderr.is_empty());
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(report_path).expect("final report must exist"),
    )
    .unwrap();
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::ChildSignaled { signal: 9 }
    );
    assert_eq!(
        report
            .signals
            .iter()
            .map(|signal| signal.signal)
            .collect::<Vec<_>>(),
        [15, 9]
    );
    assert!(report.transitions.iter().any(|transition| {
        transition.from == mlx_guard_core::PolicyState::Terminating
            && transition.to == mlx_guard_core::PolicyState::Emergency
    }));
}

#[cfg(target_os = "macos")]
#[test]
fn observe_forwards_sigterm_without_turning_it_into_policy_intervention() {
    // Catches observe mode either swallowing SIGTERM or classifying it as a memory intervention.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let journal_path = directory.0.join(".report.json.journal");
    let guard = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args(["observe", "--sample-interval", "10ms", "--report"])
        .arg(&report_path)
        .args(["--", "/bin/sleep", "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the command must run");
    wait_for_first_sample(&journal_path);
    assert!(
        Command::new("/bin/kill")
            .args(["-TERM", &guard.id().to_string()])
            .status()
            .expect("SIGTERM delivery command must run")
            .success()
    );
    let output = guard.wait_with_output().expect("supervisor must terminate");

    assert_eq!(output.status.code(), Some(143));
    assert!(output.stderr.is_empty());
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(report_path).expect("final report must exist"),
    )
    .unwrap();
    assert_eq!(
        report.configuration.mode,
        mlx_guard_core::ReportMode::Observe
    );
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::ChildSignaled { signal: 15 }
    );
    assert_eq!(report.signals.len(), 1);
    assert_eq!(report.signals[0].signal, 15);
}

#[cfg(target_os = "macos")]
#[test]
fn invalid_working_directory_is_configuration_failure_before_launch() {
    // Catches misclassifying a preflight cwd error as a runtime supervisor failure.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let missing_cwd = directory.0.join("missing");
    let marker = directory.0.join("worker-launched");
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args(["run", "--max-footprint", "1TiB", "--cwd"])
        .arg(&missing_cwd)
        .args(["--report"])
        .arg(&report_path)
        .args(["--", "/usr/bin/touch"])
        .arg(&marker)
        .output()
        .expect("the command must run");

    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "mlx-guard: child working directory is invalid\n"
    );
    assert!(!marker.exists());
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(report_path).expect("final report must exist"),
    )
    .unwrap();
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::InvalidConfiguration
    );
}

#[cfg(target_os = "macos")]
#[test]
fn late_storage_loss_does_not_stop_wall_time_intervention() {
    // Catches an artifact failure disabling the safety loop after the worker has launched.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let journal_path = directory.0.join(".report.json.journal");
    let started = Instant::now();
    let guard = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the command must run");
    let deadline = Instant::now() + Duration::from_secs(1);
    while !journal_path.exists() {
        assert!(
            Instant::now() < deadline,
            "journal initialization timed out"
        );
        thread::sleep(Duration::from_millis(1));
    }
    fs::remove_file(&journal_path).expect("the test must remove only its private journal");

    let output = guard.wait_with_output().expect("supervisor must terminate");
    assert_eq!(output.status.code(), Some(74));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "mlx-guard: artifact read failed\n"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!report_path.exists());
}

#[cfg(target_os = "macos")]
#[test]
fn policy_kills_a_worker_that_outlives_term_grace() {
    // Catches stopping escalation after a delivered TERM while the owned group remains alive.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--wall-time",
            "30ms",
            "--sample-interval",
            "10ms",
            "--report",
        ])
        .arg(&report_path)
        .args([
            "--",
            "/bin/sh",
            "-c",
            "trap '' TERM; while :; do sleep 1; done",
        ])
        .output()
        .expect("the command must run");

    assert_eq!(output.status.code(), Some(75));
    assert!(output.stderr.is_empty());
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(report_path).expect("final report must exist"),
    )
    .unwrap();
    assert_eq!(
        report
            .signals
            .iter()
            .map(|signal| signal.signal)
            .collect::<Vec<_>>(),
        [15, 9]
    );
    assert!(report.transitions.iter().any(|transition| {
        transition.from == mlx_guard_core::PolicyState::Terminating
            && transition.to == mlx_guard_core::PolicyState::Emergency
    }));
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::PolicyIntervention
    );
}

#[cfg(target_os = "macos")]
#[test]
fn launch_failures_keep_their_exact_status_and_final_report_kind() {
    // Catches collapsing not-found and not-executable launch failures into supervisor exit 70.
    let directory = TestDirectory::new();
    let not_executable = directory.0.join("not-executable");
    fs::write(&not_executable, b"not an executable").unwrap();
    let cases = [
        (
            PathBuf::from("/definitely/missing/mlx-guard-worker"),
            127,
            "mlx-guard: command was not found\n",
            mlx_guard_core::TerminalKind::LaunchNotFound,
        ),
        (
            not_executable,
            126,
            "mlx-guard: command is not executable\n",
            mlx_guard_core::TerminalKind::LaunchNotExecutable,
        ),
    ];

    for (index, (command, exit, diagnostic, terminal)) in cases.into_iter().enumerate() {
        let report_path = directory.0.join(format!("report-{index}.json"));
        let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
            .args(["run", "--max-footprint", "1TiB", "--report"])
            .arg(&report_path)
            .args(["--"])
            .arg(command)
            .output()
            .expect("the command must run");
        assert_eq!(output.status.code(), Some(exit));
        assert!(output.stdout.is_empty());
        assert_eq!(String::from_utf8_lossy(&output.stderr), diagnostic);
        let report = mlx_guard_core::ReportV1::from_json(
            &fs::read_to_string(report_path).expect("final report must exist"),
        )
        .unwrap();
        assert_eq!(report.outcome.kind, terminal);
    }
}
