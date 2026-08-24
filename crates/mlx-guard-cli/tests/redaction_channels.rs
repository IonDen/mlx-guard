#![cfg(target_os = "macos")]
//! End-to-end proof that a marker planted in any real launch channel — argv, the worker
//! executable's parent directory, the child environment, the working directory, and the child's
//! own output — reaches no persisted supervisor byte. Every case drives the built `mlx-guard`
//! binary against a real worker process and then scans the final report and its journal byte for
//! byte, so the evidence covers the whole runtime path rather than one projection function.
//!
//! Each case pins its structure before it scans: the expected exit status, a report that parses
//! as schema v1 and carries the identity projection its argv implies, a non-empty journal, and a
//! report directory holding exactly those two artifacts. A third file fails the case rather than
//! silently escaping the scan.
//!
//! Inherited child output is the one deliberate exception. The worker's stdout and stderr are
//! this process's own descriptors, so its bytes arrive unchanged by design and are not a guard
//! artifact. Cases whose worker prints a marker therefore scan only the lines the supervisor
//! itself wrote (the `mlx-guard:` prefix), and additionally assert that the worker's bytes did
//! arrive, so the narrower scan cannot pass by the worker having printed nothing at all.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Every non-intervention case supervises a whole-second sleep: long enough for the sampler to
/// observe a live process, with no fractional duration argument in play.
const SLEEP_SCRIPT: &str = "exec /bin/sleep 1";

const MARKER_ARGV: &str = "RDCT_E2E_1_c4n4ry";
const MARKER_EXECUTABLE_DIRECTORY: &str = "RDCT_E2E_2_c4n4ry";
const MARKER_ENV_KEY: &str = "RDCT_E2E_3k_c4n4ry";
const MARKER_ENV_VALUE: &str = "RDCT_E2E_3v_c4n4ry";
const MARKER_CWD: &str = "RDCT_E2E_4_c4n4ry";
const MARKER_CHILD_STDOUT: &str = "RDCT_E2E_5_c4n4ry";
const MARKER_CHILD_STDERR: &str = "RDCT_E2E_6_c4n4ry";
const MARKER_CONTROL_ARGV: &str = "RDCT_E2E_7_c4n4ry";
const MARKER_NON_UTF8_ARGV: &str = "RDCT_E2E_8_c4n4ry";
const MARKER_MULTILINE_ENV_VALUE: &str = "RDCT_E2E_9_c4n4ry";

// The all-channels case plants one marker per channel so a leak still names the channel it came
// from. The letter follows the same `RDCT_E2E_<case><tag>_c4n4ry` shape the env key/value pair
// uses: a = argv, d = executable directory, k/v = env key and value, w = working directory,
// o/e = child stdout and stderr, c = control characters, u = non-UTF-8 bytes.
const MARKER_ALL_ARGV: &str = "RDCT_E2E_10a_c4n4ry";
const MARKER_ALL_EXECUTABLE_DIRECTORY: &str = "RDCT_E2E_10d_c4n4ry";
const MARKER_ALL_ENV_KEY: &str = "RDCT_E2E_10k_c4n4ry";
const MARKER_ALL_ENV_VALUE: &str = "RDCT_E2E_10v_c4n4ry";
const MARKER_ALL_CWD: &str = "RDCT_E2E_10w_c4n4ry";
const MARKER_ALL_CHILD_STDOUT: &str = "RDCT_E2E_10o_c4n4ry";
const MARKER_ALL_CHILD_STDERR: &str = "RDCT_E2E_10e_c4n4ry";
const MARKER_ALL_CONTROL_ARGV: &str = "RDCT_E2E_10c_c4n4ry";
const MARKER_ALL_NON_UTF8_ARGV: &str = "RDCT_E2E_10u_c4n4ry";

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// A private per-case workspace: the owner-only report directory, plus room beside it for the
/// marker-named directories a case plants. Keeping fixtures out of the report directory is what
/// lets the exact-artifact assertion stay exact.
struct CaseDirectory {
    root: PathBuf,
    reports: PathBuf,
}

impl CaseDirectory {
    fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mlx-guard-redaction-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        let reports = root.join("reports");
        for path in [&root, &reports] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { root, reports }
    }

    fn report_path(&self) -> PathBuf {
        self.reports.join("report.json")
    }

    fn journal_path(&self) -> PathBuf {
        self.reports.join(".report.json.journal")
    }

    /// Create a directory whose name is the marker itself.
    fn marker_directory(&self, marker: &str) -> PathBuf {
        let path = self.root.join(marker);
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// A marker-named directory as a `--cwd` value, trailing separator included.
    fn marker_cwd(&self, marker: &str) -> OsString {
        let mut value = self.marker_directory(marker).into_os_string();
        value.push("/");
        value
    }

    /// A shebang worker inside a marker-named directory. Its basename is the only part of the
    /// path schema v1 may keep, so a surviving directory marker is unambiguously a leak.
    fn marker_worker(&self, marker: &str, script: &str) -> PathBuf {
        let path = self.marker_directory(marker).join("runner");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

impl Drop for CaseDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// The enforcement invocation every case shares.
fn guard(directory: &CaseDirectory) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mlx-guard"));
    command.args([
        "run",
        "--max-footprint",
        "1TiB",
        "--sample-interval",
        "10ms",
        "--report",
    ]);
    command.arg(directory.report_path());
    command
}

/// An argv token that wraps the marker in a terminal escape sequence and a newline.
fn control_argument(marker: &str) -> String {
    format!("\u{1b}[31m{marker}\n\u{1b}[0m")
}

/// An argv token that embeds the marker in bytes no UTF-8 decoder accepts.
fn non_utf8_argument(marker: &str) -> OsString {
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend_from_slice(marker.as_bytes());
    bytes.push(0x80);
    OsString::from_vec(bytes)
}

/// An environment value that spans lines and names the report artifact, so a leak cannot hide
/// behind either a line-oriented writer or the word it collides with.
///
/// Only the value's text is adversarial: `--env` takes `KEY=VALUE` as a `String`, so a non-UTF-8
/// environment assignment is not expressible through this interface at all.
fn multiline_env_value(marker: &str) -> String {
    format!("line1\nreport {marker}")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// The lines the supervisor itself wrote. Everything else on these streams reached them through
/// the descriptors the worker inherited.
fn supervisor_lines(stream: &[u8]) -> impl Iterator<Item = &[u8]> {
    stream
        .split(|byte| *byte == b'\n')
        .filter(|line| line.starts_with(b"mlx-guard:"))
}

/// Whether a case's worker prints marker bytes to the inherited streams.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ChildOutput {
    /// The worker prints nothing, so every byte on both streams is the supervisor's own.
    Silent,
    /// The worker prints these markers, so only supervisor-written lines may be scanned.
    Prints(&'static [&'static str]),
}

/// One case's pinned structure and the markers that must not survive anywhere.
struct Expectation {
    exit_code: i32,
    executable_basename: &'static str,
    argument_count: u32,
    markers: &'static [&'static str],
    child_output: ChildOutput,
}

/// Assert one case in contract order: exit status, parsed report, non-empty journal, exactly two
/// artifacts, then every persisted byte and every supervisor-written output byte.
fn assert_case(directory: &CaseDirectory, output: &Output, expected: &Expectation) {
    assert_eq!(
        output.status.code(),
        Some(expected.exit_code),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report_bytes = fs::read(directory.report_path()).expect("the final report must exist");
    let report_text = std::str::from_utf8(&report_bytes).expect("the report must be UTF-8");
    let report = mlx_guard_core::ReportV1::from_json(report_text)
        .expect("the report must parse as schema v1");
    assert_eq!(
        report.run.executable_basename, expected.executable_basename,
        "the legal basename channel must carry this case's worker name"
    );
    assert_eq!(
        report.run.argument_count, expected.argument_count,
        "the planted argument vector must have reached the supervisor"
    );

    let journal_bytes = fs::read(directory.journal_path()).expect("the journal must exist");
    assert!(!journal_bytes.is_empty(), "the journal must not be empty");

    let mut artifacts = fs::read_dir(&directory.reports)
        .expect("the report directory must be readable")
        .map(|entry| {
            entry
                .expect("each report directory entry must be readable")
                .file_name()
        })
        .collect::<Vec<_>>();
    artifacts.sort();
    assert_eq!(
        artifacts,
        [".report.json.journal", "report.json"].map(OsString::from),
        "the report directory must hold exactly the report and its journal"
    );

    for marker in expected.markers {
        assert!(
            !contains(&report_bytes, marker.as_bytes()),
            "the report leaked {marker}"
        );
        assert!(
            !contains(&journal_bytes, marker.as_bytes()),
            "the journal leaked {marker}"
        );
    }

    assert_output(output, expected);
}

fn assert_output(output: &Output, expected: &Expectation) {
    for (stream_name, stream) in [
        ("stdout", output.stdout.as_slice()),
        ("stderr", output.stderr.as_slice()),
    ] {
        for marker in expected.markers {
            match expected.child_output {
                ChildOutput::Silent => assert!(
                    !contains(stream, marker.as_bytes()),
                    "the supervisor's {stream_name} leaked {marker}"
                ),
                ChildOutput::Prints(_) => {
                    for line in supervisor_lines(stream) {
                        assert!(
                            !contains(line, marker.as_bytes()),
                            "a supervisor {stream_name} line leaked {marker}: {}",
                            String::from_utf8_lossy(line)
                        );
                    }
                }
            }
        }
    }

    // The narrower supervisor-line scan above only means something while the worker's own bytes
    // really do pass through these inherited streams.
    if let ChildOutput::Prints(printed) = expected.child_output {
        for marker in printed {
            assert!(
                contains(&output.stdout, marker.as_bytes())
                    || contains(&output.stderr, marker.as_bytes()),
                "the worker never printed {marker}, so its output channel went untested"
            );
        }
    }
}

#[test]
fn planted_argv_arguments_reach_no_persisted_byte() {
    // Catches a supervisor that copies the literal argument vector into its report or journal.
    let directory = CaseDirectory::new();
    let output = guard(&directory)
        .args(["--", "/bin/sh", "-c", SLEEP_SCRIPT, MARKER_ARGV])
        .arg(format!("--token={MARKER_ARGV}"))
        .output()
        .expect("the command must run");

    assert_case(
        &directory,
        &output,
        &Expectation {
            exit_code: 0,
            executable_basename: "sh",
            argument_count: 4,
            markers: &[MARKER_ARGV],
            child_output: ChildOutput::Silent,
        },
    );
}

#[test]
fn a_planted_executable_directory_reaches_no_persisted_byte() {
    // Catches persisting the worker's whole path when only its basename is a schema-v1 field.
    let directory = CaseDirectory::new();
    let worker = directory.marker_worker(
        MARKER_EXECUTABLE_DIRECTORY,
        &format!("#!/bin/sh\n{SLEEP_SCRIPT}\n"),
    );
    let output = guard(&directory)
        .arg("--")
        .arg(&worker)
        .output()
        .expect("the command must run");

    assert_case(
        &directory,
        &output,
        &Expectation {
            exit_code: 0,
            executable_basename: "runner",
            argument_count: 0,
            markers: &[MARKER_EXECUTABLE_DIRECTORY],
            child_output: ChildOutput::Silent,
        },
    );
}

#[test]
fn a_planted_child_environment_reaches_no_persisted_byte() {
    // Catches recording the child environment, whose keys name secrets as often as its values.
    let directory = CaseDirectory::new();
    let output = guard(&directory)
        .arg("--env")
        .arg(format!("{MARKER_ENV_KEY}={MARKER_ENV_VALUE}"))
        .args(["--", "/bin/sleep", "1"])
        .output()
        .expect("the command must run");

    assert_case(
        &directory,
        &output,
        &Expectation {
            exit_code: 0,
            executable_basename: "sleep",
            argument_count: 1,
            markers: &[MARKER_ENV_KEY, MARKER_ENV_VALUE],
            child_output: ChildOutput::Silent,
        },
    );
}

#[test]
fn a_planted_working_directory_reaches_no_persisted_byte() {
    // Catches persisting the validated child working directory alongside the launch record.
    let directory = CaseDirectory::new();
    let output = guard(&directory)
        .arg("--cwd")
        .arg(directory.marker_cwd(MARKER_CWD))
        .args(["--", "/bin/sleep", "1"])
        .output()
        .expect("the command must run");

    assert_case(
        &directory,
        &output,
        &Expectation {
            exit_code: 0,
            executable_basename: "sleep",
            argument_count: 1,
            markers: &[MARKER_CWD],
            child_output: ChildOutput::Silent,
        },
    );
}

#[test]
fn child_standard_output_reaches_no_persisted_byte() {
    // Catches capturing inherited worker output into a guard artifact or a supervisor message.
    let directory = CaseDirectory::new();
    let output = guard(&directory)
        .args(["--", "/bin/sh", "-c"])
        .arg(format!("echo {MARKER_CHILD_STDOUT}; {SLEEP_SCRIPT}"))
        .output()
        .expect("the command must run");

    assert_case(
        &directory,
        &output,
        &Expectation {
            exit_code: 0,
            executable_basename: "sh",
            argument_count: 2,
            markers: &[MARKER_CHILD_STDOUT],
            child_output: ChildOutput::Prints(&[MARKER_CHILD_STDOUT]),
        },
    );
}

#[test]
fn child_standard_error_reaches_no_persisted_byte() {
    // Catches treating the worker's diagnostics as supervisor diagnostics worth persisting.
    let directory = CaseDirectory::new();
    let output = guard(&directory)
        .args(["--", "/bin/sh", "-c"])
        .arg(format!("echo {MARKER_CHILD_STDERR} >&2; {SLEEP_SCRIPT}"))
        .output()
        .expect("the command must run");

    assert_case(
        &directory,
        &output,
        &Expectation {
            exit_code: 0,
            executable_basename: "sh",
            argument_count: 2,
            markers: &[MARKER_CHILD_STDERR],
            child_output: ChildOutput::Prints(&[MARKER_CHILD_STDERR]),
        },
    );
}

#[test]
fn control_wrapped_argv_reaches_no_persisted_byte() {
    // Catches a redaction path that only handles printable arguments.
    let directory = CaseDirectory::new();
    let output = guard(&directory)
        .args(["--", "/bin/sh", "-c", SLEEP_SCRIPT])
        .arg(control_argument(MARKER_CONTROL_ARGV))
        .output()
        .expect("the command must run");

    assert_case(
        &directory,
        &output,
        &Expectation {
            exit_code: 0,
            executable_basename: "sh",
            argument_count: 3,
            markers: &[MARKER_CONTROL_ARGV],
            child_output: ChildOutput::Silent,
        },
    );
}

#[test]
fn non_utf8_argv_reaches_no_persisted_byte() {
    // Catches a lossy conversion that re-encodes undecodable argv into a persisted artifact.
    let directory = CaseDirectory::new();
    let output = guard(&directory)
        .args(["--", "/bin/sh", "-c", SLEEP_SCRIPT])
        .arg(non_utf8_argument(MARKER_NON_UTF8_ARGV))
        .output()
        .expect("the command must run");

    assert_case(
        &directory,
        &output,
        &Expectation {
            exit_code: 0,
            executable_basename: "sh",
            argument_count: 3,
            markers: &[MARKER_NON_UTF8_ARGV],
            child_output: ChildOutput::Silent,
        },
    );
}

#[test]
fn a_multiline_environment_value_reaches_no_persisted_byte() {
    // Catches a line-oriented or name-matching writer that lets a multi-line value through.
    let directory = CaseDirectory::new();
    let output = guard(&directory)
        .arg("--env")
        .arg(format!(
            "TOKEN={}",
            multiline_env_value(MARKER_MULTILINE_ENV_VALUE)
        ))
        .args(["--", "/bin/sleep", "1"])
        .output()
        .expect("the command must run");

    assert_case(
        &directory,
        &output,
        &Expectation {
            exit_code: 0,
            executable_basename: "sleep",
            argument_count: 1,
            markers: &[MARKER_MULTILINE_ENV_VALUE],
            child_output: ChildOutput::Silent,
        },
    );
}

#[test]
fn every_channel_stays_redacted_through_a_policy_intervention() {
    // Catches a shutdown path that persists launch data the normal exit path redacts, since
    // signals, transitions, and the intervention outcome are all written on this path only.
    let directory = CaseDirectory::new();
    let worker = directory.marker_worker(
        MARKER_ALL_EXECUTABLE_DIRECTORY,
        &format!(
            "#!/bin/sh\necho {MARKER_ALL_CHILD_STDOUT}\necho {MARKER_ALL_CHILD_STDERR} >&2\n{SLEEP_SCRIPT}\n"
        ),
    );
    let output = guard(&directory)
        .args(["--wall-time", "300ms", "--env"])
        .arg(format!(
            "{MARKER_ALL_ENV_KEY}={}",
            multiline_env_value(MARKER_ALL_ENV_VALUE)
        ))
        .arg("--cwd")
        .arg(directory.marker_cwd(MARKER_ALL_CWD))
        .arg("--")
        .arg(&worker)
        .arg(MARKER_ALL_ARGV)
        .arg(format!("--token={MARKER_ALL_ARGV}"))
        .arg(control_argument(MARKER_ALL_CONTROL_ARGV))
        .arg(non_utf8_argument(MARKER_ALL_NON_UTF8_ARGV))
        .output()
        .expect("the command must run");

    assert_case(
        &directory,
        &output,
        &Expectation {
            exit_code: 75,
            executable_basename: "runner",
            argument_count: 4,
            markers: &[
                MARKER_ALL_ARGV,
                MARKER_ALL_EXECUTABLE_DIRECTORY,
                MARKER_ALL_ENV_KEY,
                MARKER_ALL_ENV_VALUE,
                MARKER_ALL_CWD,
                MARKER_ALL_CHILD_STDOUT,
                MARKER_ALL_CHILD_STDERR,
                MARKER_ALL_CONTROL_ARGV,
                MARKER_ALL_NON_UTF8_ARGV,
            ],
            child_output: ChildOutput::Prints(&[MARKER_ALL_CHILD_STDOUT, MARKER_ALL_CHILD_STDERR]),
        },
    );
}
