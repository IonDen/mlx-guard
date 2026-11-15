//! `--max-footprint auto` on the command line and through a real run, and the typed-size runs
//! that must stay exactly as 0.5.2 made them.
//!
//! The policy band is the one in `docs/POLICY.md`: step `max(1, limit / 10)`, warning
//! `limit - step`, recovery `limit - 2 * step`, emergency `limit + step`.

use std::process::Command;

use mlx_guard_cli::{CommandMode, parse_cli};
use mlx_guard_core::FootprintRequest;

const GUARD: &str = env!("CARGO_BIN_EXE_mlx-guard");

/// A `run` command line with this `--max-footprint` value and a harmless command.
fn run_args(max_footprint: &str) -> [&str; 8] {
    [
        "mlx-guard",
        "run",
        "--max-footprint",
        max_footprint,
        "--report",
        "/private/mlx-guard/report.json",
        "--",
        "/usr/bin/true",
    ]
}

#[test]
fn auto_parses_as_the_automatic_request() {
    // Bug: `auto` rejected, parsed as a size, or a typed size changed (rounded to a MiB) on the
    // way into the request.
    let cases = [
        ("auto", FootprintRequest::Auto),
        ("64MiB", FootprintRequest::Bytes(67_108_864)),
        ("100000007B", FootprintRequest::Bytes(100_000_007)),
        ("2B", FootprintRequest::Bytes(2)),
    ];
    for (value, expected) in cases {
        let parsed = parse_cli(run_args(value)).unwrap_or_else(|error| panic!("{value}: {error}"));
        let CommandMode::Run(run) = parsed.mode else {
            panic!("expected run mode for {value}");
        };
        assert_eq!(run.max_footprint, expected, "{value}");
    }
}

#[test]
fn auto_with_other_casing_is_a_usage_error() {
    // Bug: the word matched without regard to case or surrounding space (`eq_ignore_ascii_case`,
    // `trim`), so a wrapper's `Auto` silently turns on the automatic limit.
    for value in ["Auto", "AUTO", "auto "] {
        let error = parse_cli(run_args(value)).expect_err(value);
        assert!(!error.is_display_only(), "{value}");
        assert_eq!(error.flag(), Some("--max-footprint"), "{value}");
        let expected = format!(
            "error: invalid value '{value}' for '--max-footprint <MAX_FOOTPRINT_BYTES>': expected \
             a binary byte suffix: B, KiB, MiB, GiB, or TiB, or the word auto"
        );
        assert!(error.to_string().starts_with(&expected), "{value}: {error}");
    }

    let output = Command::new(GUARD)
        .args(&run_args("Auto")[1..])
        .output()
        .expect("the command must run");
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .starts_with("error: invalid value 'Auto' for '--max-footprint <MAX_FOOTPRINT_BYTES>'"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn size_checks_still_apply_to_a_size() {
    // Bug: the 2B minimum or the band-overflow check dropped (or applied only to `auto`) when the
    // field became a request.
    let too_small = parse_cli(run_args("1B")).expect_err("1B is under the 2B minimum");
    assert_eq!(too_small.to_string(), "--max-footprint must be at least 2B");
    assert_eq!(too_small.flag(), Some("--max-footprint"));

    // u64::MAX + u64::MAX / 10 does not fit a u64.
    let too_large = parse_cli(run_args("18446744073709551615B"))
        .expect_err("the emergency band of u64::MAX overflows");
    assert_eq!(
        too_large.to_string(),
        "--max-footprint is too large for the emergency policy band"
    );
    assert_eq!(too_large.flag(), Some("--max-footprint"));

    assert!(parse_cli(run_args("2B")).is_ok(), "2B is the minimum");

    let output = Command::new(GUARD)
        .args(&run_args("1B")[1..])
        .output()
        .expect("the command must run");
    assert_eq!(output.status.code(), Some(64));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .starts_with("--max-footprint must be at least 2B\n"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn observe_still_rejects_max_footprint() {
    // Bug: `--max-footprint` moved to the options `observe` shares, so `observe` takes a limit
    // (automatic or typed) it never enforces; or the refusal no longer says which argument was
    // wrong (any other parse error would pass a bare `is_err`).
    for value in ["auto", "64MiB"] {
        let error = parse_cli([
            "mlx-guard",
            "observe",
            "--max-footprint",
            value,
            "--report",
            "/private/mlx-guard/report.json",
            "--",
            "/usr/bin/true",
        ])
        .expect_err(&format!("observe accepted --max-footprint {value}"));
        assert!(!error.is_display_only(), "{value}");
        // clap's unexpected-argument refusal, naming the flag observe does not have. The flag
        // accessor stays empty: an unknown argument is never reported as one of the known flags.
        assert!(
            error
                .to_string()
                .starts_with("error: unexpected argument '--max-footprint' found"),
            "{value}: {error}"
        );
        assert_eq!(error.flag(), None, "{value}");
    }
}

#[cfg(target_os = "macos")]
mod real_runs {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use mlx_guard_core::warnings::auto_limit_line;
    use mlx_guard_core::{
        AutoLimitRecord, AutoLimitRule, LimitSource, Observed, PrelaunchWarning, ReportV1,
        TerminalKind, VERSION,
    };

    use super::GUARD;

    const MIB: u64 = 1 << 20;

    /// The step `docs/POLICY.md` derives from a limit: the larger of one byte and a tenth,
    /// rounded down.
    fn policy_step(limit: u64) -> u64 {
        (limit / 10).max(1)
    }

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    /// An owner-only report directory, removed when the test ends.
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mlx-guard-auto-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    /// `mlx-guard run --max-footprint <value> [extra] --report <path> -- /usr/bin/true`.
    fn run_true(max_footprint: &str, extra: &[&str], report_path: &Path) -> Output {
        Command::new(GUARD)
            .args(["run", "--max-footprint", max_footprint])
            .args(extra)
            .arg("--report")
            .arg(report_path)
            .args(["--", "/usr/bin/true"])
            .stdin(Stdio::null())
            .output()
            .expect("the command must run")
    }

    fn assert_exited_zero(output: &Output) {
        assert_eq!(
            output.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn read_report(path: &Path) -> ReportV1 {
        ReportV1::from_json(&fs::read_to_string(path).expect("final report must exist"))
            .expect("the report must parse as schema v1")
    }

    /// The 0.5.2 launch banner for a typed limit, its emergency threshold from `docs/POLICY.md`.
    fn banner_per_policy_md(limit: u64) -> String {
        format!(
            "mlx-guard: enforcing a {limit}-byte footprint limit; emergency KILL at {} bytes, \
             about 10% above the limit\n",
            limit + policy_step(limit)
        )
    }

    #[test]
    fn auto_run_records_source_rule_and_limit() {
        // Bug: the report's limit is not the one the rule chose from the recorded estimate, the
        // configuration was rebuilt from another reading (V3 then refuses the report), the band
        // is not the POLICY.md band of that limit, or the "may not fit" warning fires on a limit
        // that is always under the estimate.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let output = run_true("auto", &["--sample-interval", "10ms"], &report_path);
        assert_exited_zero(&output);

        let report = read_report(&report_path);
        assert_eq!(report.outcome.kind, TerminalKind::ChildExited { code: 0 });
        let configuration = &report.configuration;
        assert_eq!(configuration.max_footprint_source, Some(LimitSource::Auto));
        let prelaunch = report
            .prelaunch
            .as_ref()
            .expect("an auto run records its estimate");
        let Observed::Available { value: estimate } = prelaunch.memory_estimate else {
            panic!("an auto run's estimate must be available");
        };
        let limit = configuration
            .max_footprint_bytes
            .expect("an enforce report has a limit");
        assert_eq!(
            prelaunch.auto_limit,
            Some(AutoLimitRecord {
                rule: AutoLimitRule::NotWired75Percent,
                limit_bytes: limit,
            })
        );
        // floor(B * 3 / 4) without overflow, rounded down to a whole MiB.
        let expected = u128::from(estimate) * 3 / 4 / u128::from(MIB) * u128::from(MIB);
        assert_eq!(u128::from(limit), expected, "estimate {estimate}");

        let step = policy_step(limit);
        assert_eq!(configuration.warning_footprint_bytes, Some(limit - step));
        assert_eq!(
            configuration.recovery_footprint_bytes,
            Some(limit - 2 * step)
        );
        assert_eq!(configuration.emergency_footprint_bytes, Some(limit + step));
        // A documented invariant, kept on purpose: an automatic limit is at most 75 % of the
        // estimate it came from, so "may not fit" (limit above the estimate) cannot apply to it,
        // and the report a user reads must not say otherwise. It goes red only if the warning's
        // comparison breaks (say `limit > estimate` turned around), which this run then shows.
        assert!(
            !prelaunch
                .warnings
                .contains(&PrelaunchWarning::LimitMayNotFit),
            "{:?}",
            prelaunch.warnings
        );
    }

    #[test]
    fn auto_run_banner_names_the_recorded_limit() {
        // Bug: the banner (or the policy machine behind it) is fed a number other than the one
        // the report records, e.g. a second policy built from a separate reading.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let output = run_true("auto", &["--sample-interval", "10ms"], &report_path);
        assert_exited_zero(&output);

        let configuration = read_report(&report_path).configuration;
        let limit = configuration.max_footprint_bytes.unwrap();
        let emergency = configuration.emergency_footprint_bytes.unwrap();
        let text = String::from_utf8_lossy(&output.stderr);
        let first = text.lines().next().unwrap_or_default();
        assert_eq!(
            first,
            format!(
                "mlx-guard: enforcing a {limit}-byte footprint limit; emergency KILL at \
                 {emergency} bytes, about 10% above the limit"
            ),
            "{text:?}"
        );
    }

    #[test]
    fn auto_and_explicit_runs_with_the_same_number_have_identical_policy_configuration() {
        // Bug: the automatic path builds its policy differently from a typed size of the same
        // number (another band, timeout, sample bound or wall time), so `auto` is not "enforced
        // like a size you typed".
        let directory = TestDirectory::new();
        let auto_path = directory.0.join("auto.json");
        let auto = run_true("auto", &["--sample-interval", "10ms"], &auto_path);
        assert_exited_zero(&auto);
        let auto_configuration = read_report(&auto_path).configuration;
        let limit = auto_configuration.max_footprint_bytes.unwrap();

        let explicit_path = directory.0.join("explicit.json");
        let explicit = run_true(
            &format!("{limit}B"),
            &["--sample-interval", "10ms"],
            &explicit_path,
        );
        assert_exited_zero(&explicit);
        let explicit_configuration = read_report(&explicit_path).configuration;

        assert_eq!(
            auto_configuration.max_footprint_source,
            Some(LimitSource::Auto)
        );
        assert_eq!(
            explicit_configuration.max_footprint_source,
            Some(LimitSource::Explicit)
        );
        let mut auto_policy = auto_configuration;
        let mut explicit_policy = explicit_configuration;
        auto_policy.max_footprint_source = None;
        explicit_policy.max_footprint_source = None;
        assert_eq!(auto_policy, explicit_policy);

        let banner = |output: &Output| {
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned()
        };
        assert_eq!(banner(&auto), banner(&explicit));
    }

    #[test]
    fn explicit_run_records_source_explicit_and_no_auto_limit() {
        // Bug: a typed size recorded as automatic, with no source, or with an automatic record;
        // or the 0.5.2 prelaunch record lost on the way through the resolver.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let output = run_true("64MiB", &["--sample-interval", "10ms"], &report_path);
        assert_exited_zero(&output);

        let report = read_report(&report_path);
        assert_eq!(
            report.configuration.max_footprint_source,
            Some(LimitSource::Explicit)
        );
        assert_eq!(report.configuration.max_footprint_bytes, Some(67_108_864));
        let prelaunch = report
            .prelaunch
            .expect("a typed-size run still records the estimate");
        assert!(matches!(
            prelaunch.memory_estimate,
            Observed::Available { .. }
        ));
        assert_eq!(prelaunch.auto_limit, None);
    }

    #[test]
    fn explicit_run_stderr_is_byte_identical_to_0_5_2() {
        // Reference: 0.5.2's stderr, which prints the launch banner even for a command that ends
        // at once (0.5.0 could print nothing for one). Bug: an explicit run's stderr changes: a
        // launch notice meant for `auto` printed for a typed size, or the typed size rounded (to
        // a MiB) before the banner, for the aligned 64MiB and for the non-aligned 100000007B
        // alike.
        let directory = TestDirectory::new();

        let aligned_path = directory.0.join("aligned.json");
        let aligned = run_true("64MiB", &[], &aligned_path);
        assert_exited_zero(&aligned);
        assert_eq!(
            String::from_utf8_lossy(&aligned.stderr),
            "mlx-guard: enforcing a 67108864-byte footprint limit; emergency KILL at 73819750 \
             bytes, about 10% above the limit\n"
        );

        // step = floor(100000007 / 10) = 10000000, so emergency = 110000007.
        assert_eq!(policy_step(100_000_007), 10_000_000);
        let unaligned_path = directory.0.join("unaligned.json");
        let unaligned = run_true("100000007B", &[], &unaligned_path);
        assert_exited_zero(&unaligned);
        assert_eq!(
            String::from_utf8_lossy(&unaligned.stderr),
            banner_per_policy_md(100_000_007)
        );
    }

    #[test]
    fn auto_run_stderr_is_the_banner_then_the_automatic_limit_line() {
        // Bug: an automatic run prints no launch line (or the typed-size fit line in its slot),
        // prints it before the banner, or fills it with numbers other than the limit, rule and
        // estimate the report records.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let output = run_true("auto", &["--sample-interval", "10ms"], &report_path);
        assert_exited_zero(&output);

        let report = read_report(&report_path);
        let limit = report.configuration.max_footprint_bytes.unwrap();
        let prelaunch = report.prelaunch.expect("an auto run records its estimate");
        let Observed::Available { value: estimate } = prelaunch.memory_estimate else {
            panic!("an auto run's estimate must be available");
        };
        let rule = prelaunch
            .auto_limit
            .expect("an auto run records its rule")
            .rule;
        let line = auto_limit_line(rule, limit, estimate, VERSION);
        assert!(
            line.starts_with("mlx-guard: automatic limit: ")
                && line.contains("not a measure of what this command needs"),
            "{line}"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("{}{line}", banner_per_policy_md(limit))
        );
    }

    #[test]
    fn explicit_run_with_a_non_aligned_size_records_the_0_5_2_configuration() {
        // Reference: the configuration 0.5.2 records for this typed size (the same as 0.5.0's),
        // plus the new `max_footprint_source`. Bug: a typed size rounded (to a MiB, or any grid)
        // on its way to the report, or any 0.5.2 default changed while the limit moved through
        // the resolver.
        let directory = TestDirectory::new();
        let report_path = directory.0.join("report.json");
        let output = run_true("100000007B", &[], &report_path);
        assert_exited_zero(&output);

        let text = fs::read_to_string(&report_path).expect("final report must exist");
        let mut report: serde_json::Value = serde_json::from_str(&text).unwrap();
        let configuration = report["configuration"]
            .as_object_mut()
            .expect("configuration is an object");
        // How the parent is watched depends on how the test was started, not on the limit.
        configuration.remove("parent_watch");
        // Band from docs/POLICY.md with step 10000000; sample bounds at the 50ms default interval
        // (window max(50ms, 250ms), age max(100ms, 500ms)); checkpoint timeout and TERM grace 1s.
        assert_eq!(
            report["configuration"],
            serde_json::json!({
                "mode": "enforce",
                "max_footprint_bytes": 100_000_007_u64,
                "max_footprint_source": "explicit",
                "warning_footprint_bytes": 90_000_007_u64,
                "recovery_footprint_bytes": 80_000_007_u64,
                "emergency_footprint_bytes": 110_000_007_u64,
                "required_breach_samples": 2,
                "max_missing_samples": 3,
                "sample_interval_ms": 50,
                "max_sample_age_ms": 500,
                "max_sample_window_ms": 250,
                "checkpoint_timeout_ms": 1000,
                "term_grace_ms": 1000,
                "on_parent_exit": "terminate"
            })
        );
    }
}
