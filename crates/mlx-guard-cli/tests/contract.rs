use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use mlx_guard_cli::{CommandMode, OnParentExitOption, parse_bytes, parse_cli, parse_duration};

fn golden_args(contents: &str) -> Vec<OsString> {
    std::iter::once(OsString::from("mlx-guard"))
        .chain(contents.lines().map(OsString::from))
        .collect()
}

#[test]
fn run_golden_freezes_normalized_configuration_and_literal_argv() {
    // Catches unit drift, shell parsing, config-precedence drift, or reordered child argv.
    let parsed = parse_cli(golden_args(include_str!("golden/run.args")))
        .expect("the documented run command must parse");
    let CommandMode::Run(run) = parsed.mode else {
        panic!("expected run mode");
    };
    assert_eq!(run.max_footprint_bytes, 26 * 1024 * 1024 * 1024);
    assert_eq!(run.wall_time, Some(Duration::from_secs(2 * 60 * 60)));
    assert_eq!(run.checkpoint_timeout, None);
    assert_eq!(run.common.sample_interval, Duration::from_millis(50));
    assert_eq!(
        run.common.report_path,
        PathBuf::from("/private/mlx-guard/report.json")
    );
    assert_eq!(run.common.cwd, Some(PathBuf::from("/work")));
    assert_eq!(run.common.client_ready_fd, None);
    assert!(run.common.clear_env);
    assert_eq!(
        run.common.env,
        BTreeMap::from([("TOKEN".to_owned(), "redacted".to_owned())])
    );
    assert_eq!(
        run.common.command,
        ["python", "train.py", "--epochs", "2"].map(OsString::from)
    );
}

#[test]
fn observe_golden_has_no_memory_or_wall_enforcement() {
    // Catches observe mode accidentally inheriting a destructive policy option.
    let parsed = parse_cli(golden_args(include_str!("golden/observe.args")))
        .expect("the documented observe command must parse");
    let CommandMode::Observe(observe) = parsed.mode else {
        panic!("expected observe mode");
    };
    assert_eq!(observe.common.sample_interval, Duration::from_millis(100));
    assert_eq!(observe.common.client_ready_fd, None);
    assert_eq!(
        observe.common.report_path,
        PathBuf::from("/private/mlx-guard/report.json")
    );
    assert_eq!(
        observe.common.command,
        ["python", "-c", "print('ok')"].map(OsString::from)
    );
}

#[test]
fn internal_client_readiness_descriptor_is_validated_and_hidden() {
    // Catches replacing standard I/O or exposing the Python integration control as public help.
    let parsed = parse_cli([
        "mlx-guard",
        "observe",
        "--client-ready-fd",
        "9",
        "--report",
        "/private/mlx-guard/report.json",
        "--",
        "true",
    ])
    .unwrap();
    let CommandMode::Observe(observe) = parsed.mode else {
        panic!("expected observe mode");
    };
    assert_eq!(observe.common.client_ready_fd, Some(9));
    assert!(
        parse_cli([
            "mlx-guard",
            "observe",
            "--client-ready-fd",
            "2",
            "--report",
            "/private/mlx-guard/report.json",
            "--",
            "true",
        ])
        .is_err()
    );
    let help = parse_cli(["mlx-guard", "observe", "--help"]).unwrap_err();
    assert!(!help.to_string().contains("client-ready"));
}

#[test]
fn enforcement_requires_an_explicit_limit_and_mandatory_separator() {
    // Catches a universal default limit or command tokens being consumed as guard options.
    assert!(parse_cli(["mlx-guard", "run", "--", "python"]).is_err());
    assert!(parse_cli(["mlx-guard", "run", "--max-footprint", "1GiB", "python",]).is_err());
    assert!(
        parse_cli([
            "mlx-guard",
            "observe",
            "--max-footprint",
            "1GiB",
            "--",
            "python",
        ])
        .is_err()
    );
}

#[test]
fn checkpoint_timeout_is_parsed_bounded_and_enforcement_only() {
    // Catches losing the explicit acknowledgement bound, accepting a meaningless or unbounded
    // timeout, or observe mode growing an enforcement-only option.
    let parsed = parse_cli([
        "mlx-guard",
        "run",
        "--max-footprint",
        "1GiB",
        "--checkpoint-timeout",
        "250ms",
        "--report",
        "/private/mlx-guard/report.json",
        "--",
        "true",
    ])
    .expect("an in-range checkpoint timeout must parse");
    let CommandMode::Run(run) = parsed.mode else {
        panic!("expected run mode");
    };
    assert_eq!(run.checkpoint_timeout, Some(Duration::from_millis(250)));

    for out_of_range in ["9ms", "61s", "2m"] {
        let error = parse_cli([
            "mlx-guard",
            "run",
            "--max-footprint",
            "1GiB",
            "--checkpoint-timeout",
            out_of_range,
            "--report",
            "/private/mlx-guard/report.json",
            "--",
            "true",
        ])
        .unwrap_err();
        assert!(
            error.to_string().contains("10ms..=60s"),
            "value {out_of_range}: {error}"
        );
    }

    assert!(
        parse_cli([
            "mlx-guard",
            "observe",
            "--checkpoint-timeout",
            "1s",
            "--report",
            "/private/mlx-guard/report.json",
            "--",
            "true",
        ])
        .is_err()
    );
}

#[test]
fn enforcement_rejects_a_limit_too_small_for_ordered_policy_bands() {
    // Catches accepting a value that cannot satisfy recovery < warning < limit < emergency.
    let error = parse_cli([
        "mlx-guard",
        "run",
        "--max-footprint",
        "1B",
        "--report",
        "/private/mlx-guard/report.json",
        "--",
        "true",
    ])
    .unwrap_err();
    assert!(error.to_string().contains("at least 2B"));

    let error = parse_cli([
        "mlx-guard",
        "run",
        "--max-footprint",
        "18446744073709551615B",
        "--report",
        "/private/mlx-guard/report.json",
        "--",
        "true",
    ])
    .unwrap_err();
    assert!(error.to_string().contains("emergency policy band"));

    // The largest limit whose emergency band still fits in u64 is accepted and one byte more is
    // not; the Python client pins the same two literals so both grammars move together.
    for (limit, accepted) in [
        ("16769767339735956014B", true),
        ("16769767339735956015B", false),
    ] {
        let parsed = parse_cli([
            "mlx-guard",
            "run",
            "--max-footprint",
            limit,
            "--report",
            "/private/mlx-guard/report.json",
            "--",
            "true",
        ]);
        assert_eq!(parsed.is_ok(), accepted, "limit {limit}");
        if let Err(error) = parsed {
            assert!(error.to_string().contains("emergency policy band"));
        }
    }
}

#[test]
fn command_metacharacters_remain_literal_tokens() {
    // Catches introducing a shell or joining argv into one command string.
    let parsed = parse_cli([
        "mlx-guard",
        "run",
        "--max-footprint",
        "1GiB",
        "--report",
        "/private/mlx-guard/report.json",
        "--",
        "printf",
        "$(touch /tmp/must-not-exist);*",
    ])
    .expect("literal argv must parse");
    let CommandMode::Run(run) = parsed.mode else {
        panic!("expected run mode");
    };
    assert_eq!(
        run.common.command,
        ["printf", "$(touch /tmp/must-not-exist);*"].map(OsString::from)
    );
}

#[test]
fn child_environment_rejects_empty_or_duplicate_keys() {
    // Catches ambiguous last-write-wins behavior in safety-sensitive launch configuration.
    for args in [
        vec![
            "mlx-guard",
            "run",
            "--max-footprint",
            "1GiB",
            "--env",
            "=value",
            "--",
            "python",
        ],
        vec![
            "mlx-guard",
            "run",
            "--max-footprint",
            "1GiB",
            "--env",
            "A=1",
            "--env",
            "A=2",
            "--",
            "python",
        ],
    ] {
        assert!(parse_cli(args).is_err());
    }
}

#[test]
fn byte_grammar_is_binary_case_sensitive_and_overflow_checked() {
    // Catches decimal/binary ambiguity, zero limits, suffix aliases, and wrapping arithmetic.
    assert_eq!(parse_bytes("1B").unwrap(), 1);
    assert_eq!(parse_bytes("1KiB").unwrap(), 1024);
    assert_eq!(parse_bytes("26GiB").unwrap(), 26 * 1024 * 1024 * 1024);
    for invalid in [
        "0B",
        "1",
        "1KB",
        "1G",
        "1.5GiB",
        "1gib",
        "18446744073709551615TiB",
    ] {
        assert!(parse_bytes(invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn duration_grammar_is_integer_case_sensitive_and_overflow_checked() {
    // Catches implicit units, fractions, zero durations, suffix aliases, and overflow.
    assert_eq!(parse_duration("1ms").unwrap(), Duration::from_millis(1));
    assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
    assert_eq!(parse_duration("3m").unwrap(), Duration::from_secs(180));
    assert_eq!(parse_duration("4h").unwrap(), Duration::from_secs(14_400));
    for invalid in ["0s", "1", "1.5s", "1S", "-1s", "18446744073709551615h"] {
        assert!(parse_duration(invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn policy_duration_ranges_are_checked_after_unit_parsing() {
    // Catches accepting a polling interval that defeats responsiveness or an unbounded wall policy.
    for interval in ["9ms", "11s"] {
        assert!(
            parse_cli([
                "mlx-guard",
                "observe",
                "--sample-interval",
                interval,
                "--",
                "python",
            ])
            .is_err()
        );
    }
    assert!(
        parse_cli([
            "mlx-guard",
            "run",
            "--max-footprint",
            "1GiB",
            "--wall-time",
            "721h",
            "--",
            "python",
        ])
        .is_err()
    );
}

#[test]
fn on_parent_exit_defaults_to_terminate_and_is_validated() {
    // Catches losing the default terminate behavior, an unparsed detach opt-in on either mode,
    // or a typo silently falling through to whichever variant matches first.
    for (mode, args) in [
        (
            "observe",
            vec![
                "mlx-guard",
                "observe",
                "--report",
                "/private/mlx-guard/report.json",
                "--",
                "true",
            ],
        ),
        (
            "run",
            vec![
                "mlx-guard",
                "run",
                "--max-footprint",
                "1GiB",
                "--report",
                "/private/mlx-guard/report.json",
                "--",
                "true",
            ],
        ),
    ] {
        let parsed = parse_cli(args).unwrap_or_else(|error| panic!("{mode}: {error}"));
        let common = match parsed.mode {
            CommandMode::Observe(observe) => observe.common,
            CommandMode::Run(run) => run.common,
        };
        assert_eq!(
            common.on_parent_exit,
            OnParentExitOption::Terminate,
            "{mode}"
        );
    }

    let parsed = parse_cli([
        "mlx-guard",
        "observe",
        "--on-parent-exit=detach",
        "--report",
        "/private/mlx-guard/report.json",
        "--",
        "true",
    ])
    .expect("detach must parse");
    let CommandMode::Observe(observe) = parsed.mode else {
        panic!("expected observe mode");
    };
    assert_eq!(observe.common.on_parent_exit, OnParentExitOption::Detach);

    let error = parse_cli([
        "mlx-guard",
        "observe",
        "--on-parent-exit",
        "never",
        "--report",
        "/private/mlx-guard/report.json",
        "--",
        "true",
    ])
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("--on-parent-exit must be terminate or detach"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn command_argv_preserves_non_utf8_os_strings() {
    // Catches converting the supervised argv through UTF-8 or a joined command string.
    use std::os::unix::ffi::OsStringExt;

    let raw = OsString::from_vec(vec![0xff]);
    let parsed = parse_cli([
        OsString::from("mlx-guard"),
        OsString::from("observe"),
        OsString::from("--report"),
        OsString::from("/private/mlx-guard/report.json"),
        OsString::from("--"),
        raw.clone(),
    ])
    .expect("Unix argv must remain opaque");
    let CommandMode::Observe(observe) = parsed.mode else {
        panic!("expected observe mode");
    };
    assert_eq!(observe.common.command, [raw]);
}

/// The parse error for one command line, which must be rejected.
fn parse_error(args: &[&str]) -> mlx_guard_cli::CliParseError {
    parse_cli(std::iter::once("mlx-guard").chain(args.iter().copied()))
        .expect_err("this command line must be rejected")
}

#[test]
fn clap_invalid_value_yields_the_flag() {
    // Catches the flag taken as clap's whole `--max-footprint <MAX_FOOTPRINT_BYTES>` token, or
    // not taken at all, for a value clap's own parser rejected.
    for (args, flag) in [
        (
            &[
                "run",
                "--max-footprint",
                "6GB",
                "--report",
                "r.json",
                "--",
                "x",
            ][..],
            "--max-footprint",
        ),
        (
            &[
                "run",
                "--max-footprint",
                "1TiB",
                "--wall-time",
                "5x",
                "--report",
                "r.json",
                "--",
                "x",
            ][..],
            "--wall-time",
        ),
        (
            &[
                "observe",
                "--sample-interval",
                "10 ms",
                "--report",
                "r.json",
                "--",
                "x",
            ][..],
            "--sample-interval",
        ),
        (
            &[
                "run",
                "--max-footprint",
                "1TiB",
                "--on-parent-exit",
                "later",
                "--report",
                "r.json",
                "--",
                "x",
            ][..],
            "--on-parent-exit",
        ),
    ] {
        assert_eq!(parse_error(args).flag(), Some(flag), "{args:?}");
    }
}

#[test]
fn unknown_argument_yields_no_flag() {
    // Catches the whitelist being bypassed: clap's `InvalidArg` for an unknown argument is the
    // text the user typed, which must never become the flag the plain lines print.
    for args in [
        &[
            "run",
            "--RDCT_flag_c4n4ry",
            "--max-footprint",
            "1TiB",
            "--report",
            "r.json",
            "--",
            "x",
        ][..],
        &[
            "run",
            "--max-footprint=1TiB",
            "--report",
            "r.json",
            "--RDCT_c4n4ry=v",
            "--",
            "x",
        ][..],
        &["frobnicate", "--report", "r.json"][..],
    ] {
        assert_eq!(parse_error(args).flag(), None, "{args:?}");
    }
}

#[test]
fn contract_error_carries_its_flag() {
    // Catches a contract (post-clap) error that forgets which flag it is about, so the plain
    // lines fall back to "the command line" for a setting the error already names.
    for (args, flag) in [
        (
            &[
                "run",
                "--max-footprint",
                "1B",
                "--report",
                "r.json",
                "--",
                "x",
            ][..],
            "--max-footprint",
        ),
        (
            &[
                "run",
                "--max-footprint",
                "18446744073709551615B",
                "--report",
                "r.json",
                "--",
                "x",
            ][..],
            "--max-footprint",
        ),
        (
            &[
                "observe",
                "--sample-interval",
                "1ms",
                "--report",
                "r.json",
                "--",
                "x",
            ][..],
            "--sample-interval",
        ),
        (
            &["observe", "--env", "FOO", "--report", "r.json", "--", "x"][..],
            "--env",
        ),
        (
            &["observe", "--env", "=v", "--report", "r.json", "--", "x"][..],
            "--env",
        ),
        (
            &[
                "observe", "--env", "K=1", "--env", "K=2", "--report", "r.json", "--", "x",
            ][..],
            "--env",
        ),
    ] {
        let error = parse_error(args);
        assert!(!error.is_display_only(), "{args:?}");
        assert_eq!(error.flag(), Some(flag), "{args:?}: {error}");
    }
}

#[test]
fn unknown_argument_named_like_a_whitelisted_flag_yields_no_flag() {
    // Catches the whitelist being checked against clap's `InvalidArg` text without the error
    // kind: in observe mode `--max-footprint` is an unknown argument, so its typed text must be
    // dropped even though it spells a whitelisted flag.
    let error = parse_error(&[
        "observe",
        "--max-footprint",
        "1GiB",
        "--report",
        "r.json",
        "--",
        "x",
    ]);
    assert_eq!(error.flag(), None, "{error}");
}

#[test]
fn a_missing_command_after_the_separator_yields_the_command_token() {
    // Catches clap's `<COMMAND>...` token not being normalised (the trailing `...` kept), so a
    // run with nothing to supervise names no flag at all.
    for args in [
        &["run", "--max-footprint", "1TiB", "--report", "r.json"][..],
        &["run", "--max-footprint", "1TiB", "--report", "r.json", "--"][..],
        &["observe", "--report", "r.json", "--"][..],
    ] {
        assert_eq!(parse_error(args).flag(), Some("<COMMAND>"), "{args:?}");
    }
}

#[test]
fn a_repeated_report_yields_no_flag() {
    // Catches an `ArgumentConflict` error leaking the flag clap names in its context, when the
    // fix is not a value of that flag but dropping the repeat.
    let error = parse_error(&[
        "run",
        "--max-footprint",
        "1TiB",
        "--report",
        "a.json",
        "--report",
        "b.json",
        "--",
        "x",
    ]);
    assert_eq!(error.flag(), None, "{error}");
}
