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
/// The start of the launch fit line, up to the limit it names.
const FIT_PREFIX: &str = "mlx-guard: warning: your limit of ";

#[cfg(target_os = "macos")]
/// Whether `line` is the launch fit line: its exact prefix, its fixed sentences after the two
/// numbers, and the versioned link to the warnings section.
fn is_fit_line(line: &str) -> bool {
    line.starts_with(FIT_PREFIX)
        && line.ends_with(&format!(
            " macOS reported as not wired just before launch, so a run that reaches it may not \
             fit in memory on this Mac. That number is one estimate at one moment and counts \
             every other program's memory as room. mlx-guard runs with your limit as given. \
             More: {}",
            doc_link("warnings-during-a-run")
        ))
}

#[cfg(target_os = "macos")]
/// Assert a successful `run`'s stderr is exactly the one launch-banner line, optionally followed
/// by the launch fit line, and nothing else, so an unexpected diagnostic, panic fragment, or
/// stray warning on the same stream still fails the test (the strictness the old
/// `stderr.is_empty()` assertions gave, kept now that the banner is present).
fn assert_only_launch_banner(stderr: &[u8]) {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.len() == 1 || (lines.len() == 2 && is_fit_line(lines[1])),
        "a successful run's stderr must be only the launch banner and the fit line: {text:?}"
    );
    assert!(
        lines[0].contains("emergency KILL"),
        "the first stderr line must be the launch banner: {text:?}"
    );
}

/// The troubleshooting link that ends the "what to do" line, pinned to this build's release tag.
fn doc_link(anchor: &str) -> String {
    format!(
        "https://github.com/IonDen/mlx-guard/blob/v{}/docs/TROUBLESHOOTING.md#{anchor}",
        mlx_guard_core::VERSION
    )
}

/// The two plain lines exactly as stderr carries them: both prefixes, the link, a newline each.
fn plain_lines(happened: &str, to_do: &str, anchor: &str) -> String {
    format!(
        "mlx-guard: what happened: {happened}\nmlx-guard: what to do: {to_do} More: {}\n",
        doc_link(anchor)
    )
}

/// The plain lines for a command line that was not accepted, with or without a whitelisted flag.
fn command_line_lines(flag: Option<&str>) -> String {
    match flag {
        Some(flag) => plain_lines(
            &format!(
                "the command line was not accepted ({flag}), so nothing was launched and no \
                 report was written."
            ),
            &format!("fix {flag} as the error above says, then run the command again."),
            "64-the-command-line",
        ),
        None => plain_lines(
            "the command line was not accepted, so nothing was launched and no report was written.",
            "fix the command line as the error above says, then run the command again.",
            "64-the-command-line",
        ),
    }
}

#[cfg(target_os = "macos")]
/// Assert a non-zero `run`'s stderr is exactly the launch banner, optionally the launch fit line,
/// then the "what happened" line and the "what to do" line ending in the link to `anchor`.
/// Returns the two plain lines (newline-ended) so a caller can also pin their exact text.
fn assert_banner_then_plain_lines(stderr: &[u8], anchor: &str) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.len() == 4 && is_fit_line(lines[1]) {
        lines.remove(1);
    }
    assert_eq!(
        lines.len(),
        3,
        "a non-zero run's stderr must be the banner, the fit line if any, and the two plain \
         lines: {text:?}"
    );
    assert!(
        lines[0].contains("emergency KILL"),
        "the first stderr line must be the launch banner: {text:?}"
    );
    assert!(
        lines[1].starts_with("mlx-guard: what happened: "),
        "the second stderr line must say what happened: {text:?}"
    );
    assert!(
        lines[2].starts_with("mlx-guard: what to do: "),
        "the third stderr line must say what to do: {text:?}"
    );
    assert!(
        lines[2].ends_with(&format!(" More: {}", doc_link(anchor))),
        "the what-to-do line must link #{anchor}: {text:?}"
    );
    assert!(
        text.ends_with('\n'),
        "the last line must be newline-ended: {text:?}"
    );
    format!("{}\n{}\n", lines[1], lines[2])
}

#[cfg(target_os = "macos")]
/// A duration as the plain lines speak it, for the sub-minute range these runs produce: whole
/// milliseconds below one second, otherwise tenths of a second rounded half up (spec section 4).
/// Panics outside that range so a starved run fails loudly instead of being compared to a guess.
fn spoken_ms(ms: u64) -> String {
    assert!(ms < 59_950, "{ms} ms is outside the sub-minute range");
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        let tenths = (ms + 50) / 100;
        format!("{}.{} s", tenths / 10, tenths % 10)
    }
}

#[cfg(target_os = "macos")]
/// A byte count as the plain lines speak it, for the range a small real process reaches: whole
/// bytes below 1 KiB, otherwise hundredths of KiB or MiB rounded half up, stepping up to MiB when
/// the KiB figure rounds to 1024 (spec section 4). Panics at 1 GiB and above, so an unexpected
/// value fails loudly instead of being compared with a guess.
fn spoken_bytes(bytes: u64) -> String {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    assert!(
        bytes < 1 << 30,
        "{bytes} bytes is outside the KiB/MiB range"
    );
    if bytes < KIB {
        return format!("{bytes} B");
    }
    let (unit, name) = if bytes < MIB {
        let kib_hundredths = (bytes * 100 + KIB / 2) / KIB;
        if kib_hundredths < 102_400 {
            (KIB, "KiB")
        } else {
            (MIB, "MiB")
        }
    } else {
        (MIB, "MiB")
    };
    let hundredths = (bytes * 100 + unit / 2) / unit;
    format!("{}.{:02} {name}", hundredths / 100, hundredths % 100)
}

#[cfg(target_os = "macos")]
fn wait_for_first_sample(journal_path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
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
    let text = String::from_utf8_lossy(&invalid.stderr);
    assert!(text.contains("Usage: mlx-guard <COMMAND>"));
    // A missing subcommand is a rejected command line too, so clap's usage text is followed by
    // the flag-less plain lines. Red if the parse-error path of main prints none, names a flag
    // nobody gave, or prints the generic configuration row instead.
    assert!(
        text.ends_with(&command_line_lines(None)),
        "stderr must end with the flag-less command-line lines: {text:?}"
    );
}

#[test]
fn invalid_byte_unit_names_the_flag_in_the_plain_line() {
    // Catches flag extraction returning clap's whole `--max-footprint <MAX_FOOTPRINT_BYTES>`
    // token (or nothing) instead of the bare whitelisted flag.
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "6GB",
            "--report",
            "reports/r.json",
            "--",
            "/usr/bin/true",
        ])
        .output()
        .expect("the command must run");

    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    let text = String::from_utf8_lossy(&output.stderr);
    let clap_error = text
        .strip_suffix(&command_line_lines(Some("--max-footprint")))
        .unwrap_or_else(|| {
            panic!("stderr must end with the --max-footprint plain lines: {text:?}")
        });
    assert!(
        clap_error
            .starts_with("error: invalid value '6GB' for '--max-footprint <MAX_FOOTPRINT_BYTES>'"),
        "clap's own error must stay first and unchanged: {text:?}"
    );
    assert!(clap_error.ends_with('\n'), "{text:?}");
}

#[test]
fn contract_parse_error_ends_its_line_before_the_plain_lines() {
    // Catches the contract error still being printed without its newline, so the error text and
    // the "what happened" line run together on one line.
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "1B",
            "--report",
            "reports/r.json",
            "--",
            "/usr/bin/true",
        ])
        .output()
        .expect("the command must run");

    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "--max-footprint must be at least 2B\n{}",
            command_line_lines(Some("--max-footprint"))
        )
    );
}

#[test]
fn help_and_version_print_no_plain_lines() {
    // Catches the display-only parse path (help, version) getting the plain lines as if it had
    // rejected the command line.
    for args in [
        &["--help"][..],
        &["--version"][..],
        &["run", "--help"][..],
        &["observe", "--help"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
            .args(args)
            .output()
            .expect("the command must run");
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(
            output.stderr.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout.is_empty(), "{args:?} must print its text");
        assert!(
            !stdout.contains("what happened:") && !stdout.contains("what to do:"),
            "{args:?}: {stdout:?}"
        );
    }
}

#[test]
fn unknown_option_text_is_never_echoed_by_the_plain_lines() {
    // Catches the flag whitelist being bypassed: clap's `InvalidArg` for an unknown argument is
    // the user's typed text, which must stay in clap's own line and never reach the plain lines.
    let marker = "RDCT_E2E_parse_c4n4ry";
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            &format!("--{marker}"),
            "--max-footprint",
            "1TiB",
            "--report",
            "reports/r.json",
            "--",
            "/usr/bin/true",
        ])
        .output()
        .expect("the command must run");

    assert_eq!(output.status.code(), Some(64));
    let text = String::from_utf8_lossy(&output.stderr);
    let last_two: Vec<&str> = text.lines().rev().take(2).collect();
    assert!(
        last_two.iter().all(|line| !line.contains(marker)),
        "a plain line echoed the unknown option: {text:?}"
    );
    let clap_error = text
        .strip_suffix(&command_line_lines(None))
        .unwrap_or_else(|| panic!("stderr must end with the flag-less plain lines: {text:?}"));
    assert!(
        clap_error.contains(marker),
        "clap's line still names the unknown option: {text:?}"
    );
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
        format!(
            "mlx-guard: artifact directory is not private and owned\n{}",
            plain_lines(
                "the report directory is missing, not yours, or not owner-only (mode 0700), so \
                 nothing was launched.",
                "create the directory with mkdir -m 700, or fix its owner and mode, then run again.",
                "74-report-directory",
            )
        )
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
        format!(
            "mlx-guard: OS-accounted footprint observation is unsupported on this platform\n{}",
            plain_lines(
                "mlx-guard needs macOS to read the OS-accounted footprint, so it launched nothing \
                 and wrote no report.",
                "run it on a Mac with Apple Silicon.",
                "70-before-launch",
            )
        )
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
    let calibration = report
        .calibration
        .expect("an observe report must carry a calibration section");
    assert!(calibration.observation_only);
    assert!(!calibration.safety_certified);
    assert!(calibration.total_samples > 0);
    assert_eq!(
        calibration.total_samples,
        calibration.complete_samples + calibration.incomplete_samples
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
    let plain = assert_banner_then_plain_lines(&output.stderr, "75-the-wall-time-limit");
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
    // Red if the wall row names the wrong limit, or the TERM time is taken from anything but
    // the TERM record.
    let term = report
        .signals
        .iter()
        .find(|signal| signal.signal == 15)
        .expect("the TERM record");
    assert_eq!(
        plain,
        plain_lines(
            &format!(
                "the run reached the 30 ms wall-time limit you set, so mlx-guard sent SIGTERM to \
                 the command's process group at {}.",
                spoken_ms(term.at_ms)
            ),
            "raise --wall-time if the command needs longer, or find out why it ran longer than \
             expected.",
            "75-the-wall-time-limit",
        )
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
    assert_only_launch_banner(&output.stderr);
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
    assert_only_launch_banner(&output.stderr);
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
    // Red if the emergency KILL is explained as anything but the emergency row.
    let plain = assert_banner_then_plain_lines(&output.stderr, "75-emergency-kill");
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
    let emergency = report
        .transitions
        .iter()
        .find(|transition| {
            transition.from == mlx_guard_core::PolicyState::Normal
                && transition.to == mlx_guard_core::PolicyState::Emergency
        })
        .unwrap_or_else(|| panic!("no transition to emergency: {report:#?}"));
    let footprint = emergency
        .aggregate_footprint_bytes
        .unwrap_or_else(|| panic!("the emergency transition carries its sample: {report:#?}"));
    let kill_at_ms = report
        .signals
        .iter()
        .find(|signal| signal.signal == 9)
        .map_or_else(
            || panic!("no KILL record: {report:#?}"),
            |signal| signal.at_ms,
        );
    // A 2 B limit gives a 3 B emergency threshold: one tenth of 2 B rounds down to zero, and the
    // band is never narrower than 1 B. Red if the row names the wrong sample, threshold, gap,
    // limit or time, or drops "at once" or the no-grace clause.
    assert_eq!(
        plain,
        plain_lines(
            &format!(
                "one sample of the OS-accounted footprint, {}, was at or above the emergency \
                 threshold of 3 B, which is 1 B more than your 2 B limit, so mlx-guard sent \
                 SIGKILL to the command's process group at once at {}, with no checkpoint and no \
                 grace period.",
                spoken_bytes(footprint),
                spoken_ms(kill_at_ms)
            ),
            "find what allocates that much between samples, or raise --max-footprint if that \
             peak is expected; a shorter --sample-interval sees growth sooner.",
            "75-emergency-kill",
        ),
        "{report:#?}"
    );
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
    // Red if a forwarded SIGINT is told as a signal from somewhere else.
    assert_eq!(
        assert_banner_then_plain_lines(&output.stderr, "signal-passed-on-by-mlx-guard"),
        plain_lines(
            "the command ended on SIGINT, which mlx-guard received and passed on to it.",
            "nothing to do if you sent it.",
            "signal-passed-on-by-mlx-guard",
        )
    );
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
    // Red if the second-terminal-signal KILL loses its n = 9 variant.
    assert_eq!(
        assert_banner_then_plain_lines(&output.stderr, "signal-passed-on-by-mlx-guard"),
        plain_lines(
            "the command ended on SIGKILL, which mlx-guard sent after a second terminal signal \
             arrived.",
            "nothing to do if you sent it.",
            "signal-passed-on-by-mlx-guard",
        )
    );
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
    // Observe prints no banner, so the plain lines are the whole of stderr. Red if observe's
    // forwarded SIGTERM gets no lines or is explained as a policy intervention.
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        plain_lines(
            "the command ended on SIGTERM, which mlx-guard received and passed on to it.",
            "nothing to do if you sent it.",
            "signal-passed-on-by-mlx-guard",
        )
    );
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
        format!(
            "mlx-guard: child working directory is invalid\n{}",
            plain_lines(
                "mlx-guard refused a launch setting before starting the command, so nothing ran; \
                 the error line on stderr names the setting.",
                "--cwd must name an existing directory, and --env must not set \
                 MLX_GUARD_CHECKPOINT_FD; fix the one named, and give the retry a new --report \
                 name, because this run kept its report.",
                "64-a-launch-setting",
            )
        )
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
fn retry_after_a_pre_launch_failure_names_the_report_path_fix() {
    // Catches the exit-74 message for a reused report path not telling the user what to do, and a
    // retry that launches its command or overwrites the earlier report.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let missing_cwd = directory.0.join("missing");
    let marker = directory.0.join("worker-launched");
    let failed = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args(["run", "--max-footprint", "1TiB", "--cwd"])
        .arg(&missing_cwd)
        .args(["--report"])
        .arg(&report_path)
        .args(["--", "/usr/bin/touch"])
        .arg(&marker)
        .output()
        .expect("the command must run");
    assert_eq!(failed.status.code(), Some(64));
    assert!(
        report_path.exists(),
        "the pre-launch failure keeps its report"
    );

    let retry = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args(["run", "--max-footprint", "1TiB", "--report"])
        .arg(&report_path)
        .args(["--", "/usr/bin/touch"])
        .arg(&marker)
        .output()
        .expect("the command must run");

    assert_eq!(retry.status.code(), Some(74));
    assert!(retry.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&retry.stderr),
        format!(
            "mlx-guard: journal target already exists; choose a new --report path\n{}",
            plain_lines(
                "an earlier run already used this report path, and its report and hidden journal \
                 are kept, so nothing was launched.",
                "pass a new --report name, or move both the report and its hidden \
                 .<name>.journal away before reusing the path.",
                "74-report-path-already-used",
            )
        )
    );
    assert!(!marker.exists());
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(&report_path).expect("the first report must survive the retry"),
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
    let deadline = Instant::now() + Duration::from_secs(3);
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("emergency KILL"),
        "run must announce the emergency threshold at launch: {stderr}"
    );
    assert!(
        stderr.contains("mlx-guard: artifact read failed"),
        "the late-storage failure notice must still reach stderr: {stderr}"
    );
    // The diagnostic stays, and the plain lines come after it and after everything else.
    // Red if the late-74 path gets no lines, the before-launch row, or loses the failure kind.
    assert!(
        stderr.ends_with(&format!(
            "mlx-guard: artifact read failed\n{}",
            plain_lines(
                "the command was supervised, but mlx-guard could not write its complete report \
                 (reading the journal back failed); safety enforcement was not affected.",
                "check free disk space and that nothing else changed the report directory during \
                 the run; the hidden .<name>.journal beside the report path keeps what was saved. \
                 Use a new --report name next time.",
                "74-after-launch",
            )
        )),
        "the diagnostic, then the after-launch plain lines, must end stderr: {stderr}"
    );
    // Waiting out the worker's five-second sleep is the failure this catches; four seconds
    // separates it from the 100 ms wall intervention with CI scheduler headroom.
    assert!(started.elapsed() < Duration::from_secs(4));
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
    let plain = assert_banner_then_plain_lines(&output.stderr, "75-the-wall-time-limit");
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
    // Red if the KILL suffix is dropped, or its gap is measured from the outcome instead of
    // from the TERM record.
    let (term, kill) = (&report.signals[0], &report.signals[1]);
    assert_eq!(
        plain,
        plain_lines(
            &format!(
                "the run reached the 30 ms wall-time limit you set, so mlx-guard sent SIGTERM to \
                 the command's process group at {}, and SIGKILL {} later because it was still \
                 running.",
                spoken_ms(term.at_ms),
                spoken_ms(kill.at_ms - term.at_ms)
            ),
            "raise --wall-time if the command needs longer, or find out why it ran longer than \
             expected.",
            "75-the-wall-time-limit",
        )
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
fn observe_preserves_exit_status_when_child_exits_before_identity_inspection() {
    // Catches discarding a short-lived child's exit status as SupervisorFailure (exit 70) when the
    // child exits before the supervisor's first identity inspection can bind (pid, start_abstime).
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");

    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args(["observe", "--sample-interval", "10ms", "--report"])
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
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(&report_path).expect("final report must exist"),
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
fn run_preserves_nonzero_exit_status_when_child_exits_before_identity_inspection() {
    // Catches losing a non-zero child exit code when the child exits before the supervisor binds
    // its process identity.
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
        .args(["--", "/usr/bin/false"])
        .output()
        .expect("the command must run");

    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The command's own non-reserved exit code explains itself: red if the plain lines are
    // printed for it.
    assert_only_launch_banner(&output.stderr);
    let report = mlx_guard_core::ReportV1::from_json(
        &fs::read_to_string(&report_path).expect("final report must exist"),
    )
    .unwrap();
    assert_eq!(
        report.outcome.kind,
        mlx_guard_core::TerminalKind::ChildExited { code: 1 }
    );
    assert_eq!(
        report.outcome.child_status,
        Some(mlx_guard_core::ChildStatus::Exited { code: 1 })
    );
    assert!(report.signals.is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn launch_failures_keep_their_exact_status_and_final_report_kind() {
    // Catches collapsing not-found and not-executable launch failures into supervisor exit 70.
    let directory = TestDirectory::new();
    let not_executable = directory.0.join("not-executable");
    fs::write(&not_executable, b"not an executable").unwrap();
    // Red if the two launch rows are swapped, or a launch failure prints no plain lines.
    let cases = [
        (
            PathBuf::from("/definitely/missing/mlx-guard-worker"),
            127,
            format!(
                "mlx-guard: command was not found\n{}",
                plain_lines(
                    "no executable was found for the command named after --, so nothing was \
                     launched.",
                    "check the spelling, give the full path, or activate the environment that \
                     provides it; give the retry a new --report name, because this run kept its \
                     report.",
                    "127-not-found",
                )
            ),
            mlx_guard_core::TerminalKind::LaunchNotFound,
        ),
        (
            not_executable,
            126,
            format!(
                "mlx-guard: command is not executable\n{}",
                plain_lines(
                    "the file named after -- exists but could not be run, so nothing was \
                     launched.",
                    "make it executable (chmod +x) or put its interpreter first, as in -- python \
                     script.py; give the retry a new --report name, because this run kept its \
                     report.",
                    "126-not-executable",
                )
            ),
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

#[cfg(target_os = "macos")]
#[test]
#[ignore = "known bug: -- \"\" exits 70, should be 64 (command-line row)"]
fn an_empty_command_is_a_rejected_command_line() {
    // Catches an empty command after `--` reaching the run-identity step (exit 70, "could not set
    // up the run's identity") instead of being refused with the command line, before any report
    // directory work, as the 64 command-line row says ("no report was written").
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args(["run", "--max-footprint", "1TiB", "--report"])
        .arg(&report_path)
        .args(["--", ""])
        .output()
        .expect("the command must run");

    let text = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(64), "stderr: {text}");
    assert!(output.stdout.is_empty());
    assert!(
        text.ends_with(&command_line_lines(Some("the command after --"))),
        "stderr must end with the command-line lines naming the command: {text:?}"
    );
    assert!(
        !report_path.exists(),
        "a refused command line writes no report"
    );
}

#[test]
fn a_missing_command_is_named_as_the_command_after_the_separator() {
    // Catches clap's `<COMMAND>` token printed raw, or not named, in the plain lines when nothing
    // follows `--`.
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args([
            "run",
            "--max-footprint",
            "1TiB",
            "--report",
            "reports/r.json",
            "--",
        ])
        .output()
        .expect("the command must run");

    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(
        text.ends_with(&command_line_lines(Some("the command after --"))),
        "stderr must end with the missing-command plain lines: {text:?}"
    );
}

/// The warning band during a real run: the lines a run prints while its footprint sits between the
/// recovery and warning thresholds, checked against the report's own records.
#[cfg(target_os = "macos")]
mod band {
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::thread;
    use std::time::{Duration, Instant};

    use mlx_guard_core::{
        JournalEntry, JournalRecovery, Observed, PolicyState, ReportV1, TransitionRecord,
    };

    use super::{TestDirectory, assert_only_launch_banner, doc_link, spoken_bytes, spoken_ms};

    const GUARD: &str = env!("CARGO_BIN_EXE_mlx-guard");
    const ENTERED: &str = "mlx-guard: warning: ";
    const CLEARED: &str = "mlx-guard: warning cleared: ";
    /// `--max-footprint 70MiB` has a 7 MiB band step: warning 63 MiB, recovery 56 MiB, emergency
    /// 77 MiB. The allocate fixture holds 64 MiB plus a base measured at about 1.2 MiB on macOS
    /// 27.2, so the held footprint sits in the band and short of the limit.
    const LIMIT: &str = "70MiB";
    const WARNING_BYTES: u64 = 66_060_288;
    const RECOVERY_BYTES: u64 = 58_720_256;
    const LIMIT_BYTES: u64 = 73_400_320;
    /// Long enough for a 3 vCPU runner to allocate 64 MiB and sample it, and shorter than the
    /// fixture's own 9 s ceiling, so a missing line fails here and not as a fixture timeout.
    const LINE_DEADLINE: Duration = Duration::from_secs(8);

    /// The fixture lives in another crate of this workspace: `MLX_GUARD_FIXTURE` wins, otherwise it
    /// sits next to the supervisor in the target directory, where a workspace build puts it.
    fn fixture() -> PathBuf {
        let path = std::env::var_os("MLX_GUARD_FIXTURE").map_or_else(
            || Path::new(GUARD).with_file_name("mlx-guard-fixture"),
            PathBuf::from,
        );
        assert!(
            path.is_file(),
            "no fixture at {}; run `cargo build -p mlx-guard-test-support --bin mlx-guard-fixture`",
            path.display()
        );
        path
    }

    /// A supervised allocate fixture driven through its stdin, with stderr read line by line as
    /// it arrives. Dropping it closes stdin, which ends the fixture, and then kills a supervisor
    /// that has not exited, so a failed test leaves nothing running.
    struct BandRun {
        child: Child,
        stdin: Option<ChildStdin>,
        lines: Receiver<String>,
        seen: Vec<String>,
        journal_path: PathBuf,
    }

    impl BandRun {
        fn start(mode: &[&str], report_path: &Path) -> Self {
            let mut child = Command::new(GUARD)
                .args(mode)
                .args(["--sample-interval", "10ms", "--report"])
                .arg(report_path)
                .arg("--")
                .arg(fixture())
                .args(["allocate", "67108864", "9000"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("the supervisor must start");
            let stdin = child.stdin.take();
            let mut stderr = BufReader::new(child.stderr.take().expect("stderr is piped"));
            let (sender, lines) = mpsc::channel();
            thread::spawn(move || {
                let mut line = Vec::new();
                while stderr
                    .read_until(b'\n', &mut line)
                    .is_ok_and(|read| read > 0)
                {
                    if sender
                        .send(String::from_utf8_lossy(&line).into_owned())
                        .is_err()
                    {
                        return;
                    }
                    line.clear();
                }
            });
            let journal_name = format!(
                ".{}.journal",
                report_path.file_name().unwrap().to_string_lossy()
            );
            Self {
                child,
                stdin,
                lines,
                seen: Vec::new(),
                journal_path: report_path.with_file_name(journal_name),
            }
        }

        fn send(&mut self, command: &str) {
            let stdin = self.stdin.as_mut().expect("stdin is open until finish");
            stdin.write_all(format!("{command}\n").as_bytes()).unwrap();
        }

        /// The first stderr line starting with `prefix`, newline included, waiting at most
        /// `LINE_DEADLINE` for it while the run is still going.
        fn wait_for_line(&mut self, prefix: &str) -> String {
            if let Some(line) = self.seen.iter().find(|line| line.starts_with(prefix)) {
                return line.clone();
            }
            let deadline = Instant::now() + LINE_DEADLINE;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.lines.recv_timeout(left) {
                    Ok(line) => {
                        self.seen.push(line.clone());
                        if line.starts_with(prefix) {
                            return line;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => panic!(
                        "no {prefix:?} line within {LINE_DEADLINE:?}; stderr so far: {:?}",
                        self.seen
                    ),
                    Err(RecvTimeoutError::Disconnected) => {
                        panic!("stderr ended with no {prefix:?} line: {:?}", self.seen)
                    }
                }
            }
        }

        /// Wait until the journal holds a sample that the predicate accepts.
        fn wait_for_sample(&self, accept: impl Fn(u64) -> bool) {
            let deadline = Instant::now() + LINE_DEADLINE;
            loop {
                if let Ok(recovery) = JournalRecovery::read(&self.journal_path)
                    && recovery.records.iter().any(|record| {
                        matches!(
                            &record.entry,
                            JournalEntry::Sample(window) if matches!(
                                window.aggregate_footprint_bytes,
                                Observed::Available { value } if accept(value)
                            )
                        )
                    })
                {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "no such sample within {LINE_DEADLINE:?}"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }

        /// Close stdin after `exit`, wait for the supervisor, and return its exit code with every
        /// stderr line it wrote.
        fn finish(&mut self) -> (Option<i32>, Vec<String>) {
            self.send("exit");
            self.stdin = None;
            let status = self
                .child
                .wait()
                .expect("the supervisor must be waited for");
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.lines.recv_timeout(left) {
                    Ok(line) => self.seen.push(line),
                    Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => panic!("stderr did not reach end of file"),
                }
            }
            (status.code(), self.seen.clone())
        }
    }

    impl Drop for BandRun {
        fn drop(&mut self) {
            self.stdin = None;
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.child.try_wait().ok().flatten().is_none() {
                if Instant::now() >= deadline {
                    self.child.kill().ok();
                    self.child.wait().ok();
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn read_report(path: &Path) -> ReportV1 {
        ReportV1::from_json(&fs::read_to_string(path).expect("the final report must exist"))
            .expect("the final report must validate")
    }

    fn transition(report: &ReportV1, from: PolicyState, to: PolicyState) -> &TransitionRecord {
        report
            .transitions
            .iter()
            .find(|record| record.from == from && record.to == to)
            .unwrap_or_else(|| panic!("no {from:?} to {to:?} record: {:?}", report.transitions))
    }

    #[test]
    fn a_run_in_the_band_prints_the_entered_line_matching_the_transition() {
        // Bug: the line and the report disagree: the line reads a different clock, or prints the
        // previous sample's footprint instead of the one that crossed.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let mut run = BandRun::start(&["run", "--max-footprint", LIMIT], &report_path);
        run.send("allocate");
        let line = run.wait_for_line(ENTERED);
        run.send("release");
        let (code, stderr) = run.finish();

        assert_eq!(code, Some(0), "stderr: {stderr:?}");
        let report = read_report(&report_path);
        let entered = transition(&report, PolicyState::Normal, PolicyState::Warning);
        let footprint = entered
            .aggregate_footprint_bytes
            .expect("the crossing record carries its sample");
        assert!(
            (WARNING_BYTES..LIMIT_BYTES).contains(&footprint),
            "the crossing sample must sit in the band: {footprint}"
        );
        assert_eq!(
            line,
            format!(
                "mlx-guard: warning: the OS-accounted footprint reached {} at {}, at or above the \
                 63.00 MiB warning threshold for your 70.00 MiB limit; 2 samples in a row at or \
                 above the limit stop the command, and 1 sample at or above the 77.00 MiB \
                 emergency threshold kills it at once, with no checkpoint. More: {}\n",
                spoken_bytes(footprint),
                spoken_ms(entered.at_ms),
                doc_link("warnings-during-a-run")
            )
        );
    }

    #[test]
    fn a_released_allocation_prints_the_cleared_line() {
        // Bug: recovery is wired to the wrong state, so the warning never clears on stderr and the
        // user is left believing the footprint is still in the band.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let mut run = BandRun::start(&["run", "--max-footprint", LIMIT], &report_path);
        run.send("allocate");
        run.wait_for_line(ENTERED);
        run.send("release");
        // Waited for while the command still runs, so the line is shown to arrive before exit.
        let line = run.wait_for_line(CLEARED);
        let (code, stderr) = run.finish();

        assert_eq!(code, Some(0), "stderr: {stderr:?}");
        let report = read_report(&report_path);
        let cleared = transition(&report, PolicyState::Warning, PolicyState::Normal);
        let footprint = cleared
            .aggregate_footprint_bytes
            .expect("the recovery record carries its sample");
        assert!(
            footprint <= RECOVERY_BYTES,
            "the recovery sample must be at or below the recovery threshold: {footprint}"
        );
        assert_eq!(
            line,
            format!(
                "mlx-guard: warning cleared: the OS-accounted footprint fell to {} at {}, at or \
                 below the 56.00 MiB recovery threshold for your 70.00 MiB limit.\n",
                spoken_bytes(footprint),
                spoken_ms(cleared.at_ms)
            )
        );
    }

    #[test]
    fn a_run_that_never_reaches_the_band_prints_no_warning() {
        // Bug: a band line on an ordinary run, one whose footprint never reaches the warning
        // threshold.
        let directory = TestDirectory::new();
        let output = Command::new(GUARD)
            .args([
                "run",
                "--max-footprint",
                "64MiB",
                "--sample-interval",
                "10ms",
            ])
            .arg("--report")
            .arg(directory.0.join("report.json"))
            .args(["--", "/bin/sleep", "1"])
            .stdin(Stdio::null())
            .output()
            .expect("the command must run");

        assert_eq!(
            output.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_only_launch_banner(&output.stderr);
    }

    #[test]
    fn observe_prints_no_band_line() {
        // Bug: the band leaks into `observe`, which has no limit and so no band to report.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let mut run = BandRun::start(&["observe"], &report_path);
        run.send("allocate");
        // The same footprint that enters the band under `run --max-footprint 70MiB`, then a few
        // more samples, so a line printed at the crossing has had time to arrive.
        run.wait_for_sample(|value| value >= WARNING_BYTES);
        thread::sleep(Duration::from_millis(200));
        run.send("release");
        let (code, stderr) = run.finish();

        assert_eq!(code, Some(0), "stderr: {stderr:?}");
        assert!(stderr.is_empty(), "observe wrote to stderr: {stderr:?}");
        assert_eq!(
            read_report(&report_path).configuration.mode,
            mlx_guard_core::ReportMode::Observe
        );
    }
}

/// The launch fit line: one warning after the banner when the limit is more than the memory
/// macOS reported as not wired just before launch.
#[cfg(target_os = "macos")]
mod fit {
    use std::fs;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};

    use mlx_guard_core::explain::format_bytes;
    use mlx_guard_core::{Observed, PrelaunchRecord, PrelaunchWarning, ReportMode, ReportV1};

    use super::{FIT_PREFIX, TestDirectory, doc_link, plain_lines};

    const GUARD: &str = env!("CARGO_BIN_EXE_mlx-guard");
    /// A limit no Mac this runs on has as memory, so the line must print.
    const BEYOND: &str = "1TiB";

    fn read_report(path: &Path) -> ReportV1 {
        ReportV1::from_json(&fs::read_to_string(path).expect("final report must exist"))
            .expect("the report must parse as schema v1")
    }

    fn guard(mode: &[&str], report_path: &Path, command: &[&str]) -> Output {
        Command::new(GUARD)
            .args(mode)
            .args(["--sample-interval", "10ms", "--report"])
            .arg(report_path)
            .arg("--")
            .args(command)
            .stdin(Stdio::null())
            .output()
            .expect("the command must run")
    }

    /// The estimate the report recorded, which must be a number.
    fn recorded_estimate(prelaunch: &PrelaunchRecord) -> u64 {
        match prelaunch.memory_estimate {
            Observed::Available { value } => value,
            ref other => panic!("the estimate must be available on macOS: {other:?}"),
        }
    }

    /// The fit line for a 1 TiB limit, with the estimate the report recorded. 1 TiB prints as
    /// GiB, and no Mac's estimate renders like it, so the short form is the one expected.
    fn beyond_line(estimate: u64) -> String {
        format!(
            "{FIT_PREFIX}1024.00 GiB is more than the {} macOS reported as not wired just before \
             launch, so a run that reaches it may not fit in memory on this Mac. That number is \
             one estimate at one moment and counts every other program's memory as room. \
             mlx-guard runs with your limit as given. More: {}",
            format_bytes(estimate),
            doc_link("warnings-during-a-run")
        )
    }

    #[test]
    fn a_limit_beyond_this_machine_prints_the_fit_line_and_still_runs() {
        // Bug: the check refuses the run, changes its exit status, prints the line without
        // recording the warning, or prints a number other than the one recorded.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let output = guard(
            &["run", "--max-footprint", BEYOND],
            &report_path,
            &["/bin/sh", "-c", "exit 3"],
        );

        assert_eq!(
            output.status.code(),
            Some(3),
            "the command's own status: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = read_report(&report_path);
        let prelaunch = report
            .prelaunch
            .expect("a run report must carry the launch estimate");
        assert_eq!(prelaunch.warnings, [PrelaunchWarning::LimitMayNotFit]);
        let estimate = recorded_estimate(&prelaunch);
        let text = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "the banner, then the fit line: {text:?}");
        assert!(lines[0].contains("emergency KILL"), "{text:?}");
        assert_eq!(lines[1], beyond_line(estimate));
        assert!(text.ends_with('\n'), "{text:?}");
        assert_eq!(
            report.outcome.kind,
            mlx_guard_core::TerminalKind::ChildExited { code: 3 }
        );
    }

    #[test]
    fn a_modest_limit_prints_no_fit_line() {
        // Bug: the line printed on every run, whatever the limit and the estimate.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let output = guard(
            &["run", "--max-footprint", "64MiB"],
            &report_path,
            &["/usr/bin/true"],
        );

        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "the banner alone: {text:?}");
        assert!(lines[0].contains("emergency KILL"), "{text:?}");
        let prelaunch = read_report(&report_path)
            .prelaunch
            .expect("a run report records the estimate even when no line prints");
        assert!(recorded_estimate(&prelaunch) > 64 << 20);
        assert!(prelaunch.warnings.is_empty());
    }

    #[test]
    fn observe_prints_no_fit_line_and_records_no_prelaunch() {
        // Bug: the check leaks into `observe`, which has no limit to compare.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let output = guard(&["observe"], &report_path, &["/usr/bin/true"]);

        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "observe wrote to stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = read_report(&report_path);
        assert_eq!(report.configuration.mode, ReportMode::Observe);
        assert_eq!(report.prelaunch, None);
    }

    #[test]
    fn a_failed_launch_prints_no_fit_line_but_records_the_number() {
        // Bug: the line announces a command that never ran, or a failed launch loses the number
        // measured before it.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let output = guard(
            &["run", "--max-footprint", BEYOND],
            &report_path,
            &["/definitely/missing/mlx-guard-worker"],
        );

        assert_eq!(output.status.code(), Some(127));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "mlx-guard: command was not found\n{}",
                plain_lines(
                    "no executable was found for the command named after --, so nothing was \
                     launched.",
                    "check the spelling, give the full path, or activate the environment that \
                     provides it; give the retry a new --report name, because this run kept its \
                     report.",
                    "127-not-found",
                )
            )
        );
        let prelaunch = read_report(&report_path)
            .prelaunch
            .expect("a failed launch still records the estimate");
        assert!(recorded_estimate(&prelaunch) > 0);
        assert_eq!(prelaunch.warnings, [PrelaunchWarning::LimitMayNotFit]);
    }
}
