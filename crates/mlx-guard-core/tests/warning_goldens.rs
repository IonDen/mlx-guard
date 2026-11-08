//! Golden tests for the launch fit line and the three warning-band lines printed during a run.
//!
//! Every row of `fixtures/explain/warnings.json` names a line kind, the arguments it is rendered
//! from, and the exact text without the `mlx-guard: ` prefix and the newline. The link carries the
//! literal `{version}` placeholder, filled with the version the test passes. The Python page test
//! reads the same corpus, so a quote on the troubleshooting page that drifts from these lines
//! turns it red.

use std::fs;
use std::path::{Path, PathBuf};

use mlx_guard_core::VERSION;
use mlx_guard_core::explain::ANCHORS;
use mlx_guard_core::warnings::{
    WARNING_ANCHOR, band_cleared_line, band_entered_line, band_still_line, fit_line,
};
use serde::Deserialize;

const WARNINGS_JSON: &str = "crates/mlx-guard-core/tests/fixtures/explain/warnings.json";
/// The largest write POSIX promises is not interleaved on a pipe (`PIPE_BUF` on Darwin).
const ATOMIC_PIPE_WRITE: usize = 512;
/// A long release-candidate version, so the link is as long as a real release can make it.
const LONG_VERSION: &str = "10.10.10-rc.10";
const MAX_BYTES_TEXT: &str = "18446744073709551615 bytes";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnteredArgs {
    footprint: u64,
    at_ms: u64,
    warning: u64,
    limit: u64,
    emergency: u64,
    required_samples: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StillArgs {
    footprint: u64,
    at_ms: u64,
    since_ms: u64,
    recovery: u64,
    limit: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FitArgs {
    limit: u64,
    estimate: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClearedArgs {
    footprint: u64,
    at_ms: u64,
    recovery: u64,
    limit: u64,
}

/// One `warnings.json` row. `note` is for readers; the tests ignore it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    row: String,
    kind: String,
    #[allow(dead_code)]
    note: Option<String>,
    args: serde_json::Value,
    text: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cases() -> Vec<Case> {
    let path = repo_root().join(WARNINGS_JSON);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text).expect("warnings.json must be an array of {row, kind, args, text}")
}

fn args<T: for<'de> Deserialize<'de>>(row: &Case) -> T {
    serde_json::from_value(row.args.clone())
        .unwrap_or_else(|error| panic!("row {}: bad args for {}: {error}", row.row, row.kind))
}

fn render(row: &Case, version: &str) -> String {
    match row.kind.as_str() {
        "entered" => {
            let a: EnteredArgs = args(row);
            band_entered_line(
                a.footprint,
                a.at_ms,
                a.warning,
                a.limit,
                a.emergency,
                a.required_samples,
                version,
            )
        }
        "still" => {
            let a: StillArgs = args(row);
            band_still_line(a.footprint, a.at_ms, a.since_ms, a.recovery, a.limit)
        }
        "cleared" => {
            let a: ClearedArgs = args(row);
            band_cleared_line(a.footprint, a.at_ms, a.recovery, a.limit)
        }
        "fit" => {
            let a: FitArgs = args(row);
            fit_line(a.limit, a.estimate, version)
        }
        other => panic!("row {}: unknown kind {other}", row.row),
    }
}

fn expected(row: &Case, version: &str) -> String {
    format!("mlx-guard: {}\n", row.text.replace("{version}", version))
}

fn find_row(row_id: &str) -> Case {
    let mut matching: Vec<Case> = cases()
        .into_iter()
        .filter(|row| row.row == row_id)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "warnings.json must hold one row {row_id}"
    );
    matching.remove(0)
}

/// The three band lines at the largest values each argument can hold, as two forms: every value
/// equal (rendered in GiB), and every compared value distinct but rendering alike, which forces
/// the longer `{n} bytes` form.
fn extreme_band_lines() -> Vec<String> {
    let max = u64::MAX;
    vec![
        band_entered_line(max, max, max, max, max, u32::MAX, LONG_VERSION),
        band_entered_line(max, max, max - 1, max - 2, max - 3, u32::MAX, LONG_VERSION),
        band_still_line(max, max, max, max, max),
        band_still_line(max, max, max, max - 1, max - 2),
        band_cleared_line(max, max, max, max),
        band_cleared_line(max, max, max - 1, max - 2),
    ]
}

/// The fit line at the same two extremes: both values `u64::MAX`, and two distinct values that
/// render alike, which forces the `{n} bytes` form for both.
fn extreme_fit_lines() -> Vec<String> {
    let max = u64::MAX;
    vec![
        fit_line(max, max, LONG_VERSION),
        fit_line(max, max - 1, LONG_VERSION),
    ]
}

fn extreme_lines() -> Vec<String> {
    let mut lines = extreme_band_lines();
    lines.extend(extreme_fit_lines());
    lines
}

/// The lines whose compared values render alike, used by two tests.
fn small_limit_lines() -> [String; 3] {
    [
        band_entered_line(9217, 1500, 9216, 10240, 11264, 2, VERSION),
        band_still_line(8193, 301_500, 1500, 8192, 10240),
        band_cleared_line(8191, 302_000, 8192, 10240),
    ]
}

#[test]
fn every_corpus_row_renders_exactly() {
    // Bug: any wording drift between the renderers, the corpus and the page (one changed word,
    // a dropped clause, a missing newline, "warning:" on the cleared line).
    let cases = cases();
    let kinds: Vec<&str> = cases.iter().map(|row| row.kind.as_str()).collect();
    for kind in ["entered", "still", "cleared", "fit"] {
        assert!(kinds.contains(&kind), "warnings.json has no {kind} row");
    }
    for row in &cases {
        for version in [VERSION, LONG_VERSION] {
            assert_eq!(
                render(row, version),
                expected(row, version),
                "row {} ({version})",
                row.row
            );
        }
    }
}

#[test]
fn the_entered_line_links_its_own_anchor() {
    // Bug: the link built with an explain anchor, or WARNING_ANCHOR added to explain::ANCHORS,
    // which Python mirrors line for line from anchors.txt.
    assert_eq!(WARNING_ANCHOR, "warnings-during-a-run");
    assert!(!ANCHORS.contains(&WARNING_ANCHOR));
    let row = find_row("entered");
    assert!(
        render(&row, "0.5.0").ends_with(
            " More: https://github.com/IonDen/mlx-guard/blob/v0.5.0/docs/TROUBLESHOOTING.md\
             #warnings-during-a-run\n"
        ),
        "the entered line must end with the versioned link"
    );
}

#[test]
fn a_line_at_the_largest_values_fits_one_atomic_pipe_write() {
    // Bug: a line longer than PIPE_BUF, which can interleave with the command's own output, or
    // a line split in two writes by an embedded newline.
    let lines = extreme_lines();
    assert!(
        lines[1].contains(MAX_BYTES_TEXT),
        "the distinct near-maximum case must reach the bytes form: {}",
        lines[1]
    );
    let fit_bytes_form = &extreme_fit_lines()[1];
    assert!(
        fit_bytes_form.contains(
            "your limit of 18446744073709551615 bytes is more than the \
                                 18446744073709551614 bytes macOS"
        ),
        "the near-maximum fit line must reach the bytes form: {fit_bytes_form}"
    );
    for line in &lines {
        assert!(
            line.len() <= ATOMIC_PIPE_WRITE,
            "{} bytes: {line}",
            line.len()
        );
        assert!(line.ends_with('\n'), "not newline-ended: {line:?}");
        assert_eq!(
            line.matches('\n').count(),
            1,
            "more than one line: {line:?}"
        );
    }
    let longest = lines.iter().map(String::len).max().unwrap_or(0);
    println!("longest line: {longest} bytes");
    println!("longest fit line: {} bytes", fit_bytes_form.len());
}

#[test]
fn fit_line_never_says_more_than_between_equal_renderings() {
    // Bug: the fit line formats its two numbers separately instead of through `compared_bytes`,
    // so a limit one byte over the estimate reads "30.00 GiB is more than the 30.00 GiB".
    assert_eq!(
        fit_line(32_212_254_721, 32_212_254_720, "0.5.0"),
        "mlx-guard: warning: your limit of 32212254721 bytes is more than the 32212254720 bytes \
         macOS reported as not wired just before launch, so a run that reaches it may not fit in \
         memory on this Mac. That number is one estimate at one moment and counts every other \
         program's memory as room. mlx-guard runs with your limit as given. More: \
         https://github.com/IonDen/mlx-guard/blob/v0.5.0/docs/TROUBLESHOOTING.md\
         #warnings-during-a-run\n"
    );
    // Values that render apart keep the short form.
    let apart = fit_line(1_099_511_627_776, 29_678_236_467, "0.5.0");
    assert!(
        apart.starts_with(
            "mlx-guard: warning: your limit of 1024.00 GiB is more than the 27.64 GiB macOS"
        ),
        "{apart}"
    );
    // Whatever the pair, the two rendered numbers differ.
    for (limit, estimate) in [
        (1024, 1023),
        (1025, 1024),
        (1_048_577, 1_048_576),
        (1_073_741_825, 1_073_741_824),
        (u64::MAX, u64::MAX - 1),
    ] {
        let line = fit_line(limit, estimate, "0.5.0");
        let (_, rest) = line
            .split_once("your limit of ")
            .expect("the line names the limit");
        let (limit_text, rest) = rest
            .split_once(" is more than the ")
            .expect("the line compares the limit");
        let (estimate_text, _) = rest
            .split_once(" macOS reported")
            .expect("the line names the estimate");
        assert_ne!(limit_text, estimate_text, "{line}");
    }
}

#[test]
fn small_limits_print_exact_bytes_when_values_render_alike() {
    // Bug: "reached 9.00 KiB, at or above the 9.00 KiB warning threshold" for two different
    // values, or the emergency threshold left out of the comparison.
    let [entered, still, cleared] = small_limit_lines();
    assert_eq!(
        entered,
        format!(
            "mlx-guard: warning: the OS-accounted footprint reached 9217 bytes at 1.5 s, at or \
             above the 9216 bytes warning threshold for your 10240 bytes limit; 2 samples in a row \
             at or above the limit stop the command, and 1 sample at or above the 11264 bytes \
             emergency threshold kills it at once, with no checkpoint. More: \
             https://github.com/IonDen/mlx-guard/blob/v{VERSION}/docs/TROUBLESHOOTING.md\
             #warnings-during-a-run\n"
        )
    );
    assert_eq!(
        still,
        "mlx-guard: warning: the OS-accounted footprint is 8193 bytes at 5 min 2 s and has not \
         fallen to the 8192 bytes recovery threshold since 1.5 s; your limit is 10240 bytes.\n"
    );
    assert_eq!(
        cleared,
        "mlx-guard: warning cleared: the OS-accounted footprint fell to 8191 bytes at 5 min 2 s, \
         at or below the 8192 bytes recovery threshold for your 10240 bytes limit.\n"
    );
    // Only the emergency threshold renders like the limit (6.00 GiB and 6.00 GiB + 1 byte).
    let emergency_alike = band_entered_line(
        5_819_652_998,
        12_300,
        5_798_205_850,
        6_442_450_944,
        6_442_450_945,
        2,
        VERSION,
    );
    assert!(
        emergency_alike.contains("for your 6442450944 bytes limit;")
            && emergency_alike.contains("at or above the 6442450945 bytes emergency threshold"),
        "{emergency_alike}"
    );
}

#[test]
fn the_entered_line_names_the_emergency_threshold() {
    // Bug: the line promises two samples while one sample at the emergency threshold ends the
    // run at once.
    let row = find_row("entered");
    let line = render(&row, VERSION);
    assert!(
        line.contains(
            "1 sample at or above the 6.60 GiB emergency threshold kills it at once, with no \
             checkpoint"
        ),
        "{line}"
    );
}

/// The part of a line a reader sees as prose: no prefix, no link.
fn prose(line: &str) -> &str {
    let line = line.trim_end_matches('\n');
    line.split_once(" More: ").map_or(line, |(prose, _)| prose)
}

#[test]
fn the_wording_rule_holds_in_every_line() {
    // Bug: a rewording that breaks the public rule: a bare "above" for a threshold, "hard
    // boundary", "real memory", a footprint not called OS-accounted, a path in the text, or a fit
    // line that promises the run will not fit.
    let cases = cases();
    let mut band_lines: Vec<String> = cases
        .iter()
        .filter(|row| row.kind != "fit")
        .map(|row| render(row, VERSION))
        .collect();
    band_lines.extend(small_limit_lines());
    band_lines.extend(extreme_band_lines());
    let mut fit_lines: Vec<String> = cases
        .iter()
        .filter(|row| row.kind == "fit")
        .map(|row| render(row, VERSION))
        .collect();
    fit_lines.extend(extreme_fit_lines());
    for line in &fit_lines {
        let text = prose(line);
        assert!(text.contains(" may not fit in memory "), "{text}");
        assert!(!text.to_lowercase().contains("will not fit"), "{text}");
    }
    for line in band_lines.iter().chain(&fit_lines) {
        let text = prose(line);
        for (index, _) in text.match_indices("above") {
            assert!(
                text[..index].ends_with("at or "),
                "a bare \"above\" at byte {index}: {text}"
            );
        }
        let lower = text.to_lowercase();
        assert!(!lower.contains("hard"), "{text}");
        assert!(!lower.contains("real memory"), "{text}");
        assert!(!text.contains('/'), "a path outside the link: {text}");
    }
    for line in &band_lines {
        let text = prose(line);
        assert!(text.contains("the OS-accounted footprint "), "{text}");
    }
}
