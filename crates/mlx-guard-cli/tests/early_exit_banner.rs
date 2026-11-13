#![cfg(target_os = "macos")]

//! A command that has already exited when `run` first looks at it must still get the launch
//! banner, through the real binary.
//!
//! `run` announces the ceiling it enforces once the command is launched. A command that finishes
//! before the supervisor inspects it has been launched too, so its run owes the same line. Whether
//! a real command finishes inside that window depends on scheduling: a fast command almost never
//! does on an idle Mac, and sometimes does on a loaded CI runner. This test removes the luck.
//!
//! The supervisor runs with a small interposer library on `DYLD_INSERT_LIBRARIES`. It replaces
//! `getpgid`: the first call that names one of the supervisor's own children waits, with
//! `waitid(WEXITED | WNOWAIT)`, until that child has exited, leaves it unreaped, and only then
//! makes the real call. The supervisor's first look at a launched command is that `getpgid`, so
//! every run reaches its first post-launch step with the command already gone, as a run on a
//! descheduled supervisor does. No load, no sleep and no source change is involved: the library
//! only delays one call until a condition holds, and it writes a marker when it did, so a run in
//! which the hold never engaged fails as such instead of passing on the ordinary path.
//!
//! The library is compiled from the C source below with the system `cc`, the same tool the Rust
//! toolchain links with on macOS, into the test's own scratch directory. A missing or failing
//! `cc` fails the test; it never skips.
//!
//! The library belongs in the supervisor only. Every run here passes `--clear-env`, so the
//! supervised command starts without `DYLD_INSERT_LIBRARIES`: dyld aborts an arm64e system binary
//! such as `/bin/echo` that is asked to load this arm64 library wherever the variable is not
//! stripped from it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_core::warnings::fit_line;
use mlx_guard_core::{ChildStatus, Observed, ReportV1, TerminalKind};

const GUARD: &str = env!("CARGO_BIN_EXE_mlx-guard");
/// What the command prints; the run's stdout must start with it unchanged.
const STDOUT_TOKEN: &str = "early-exit-stdout";
/// The words every launch banner carries (`run` announces the emergency KILL threshold).
const BANNER_WORDS: &str = "emergency KILL";
/// The limit most runs enforce, and its byte count as the banner spells it (1 GiB = 2^30 bytes).
/// It is below the launch estimate on any Mac that can run the tests, so it gets no fit line.
const LIMIT: &str = "1GiB";
const LIMIT_IN_BANNER: &str = "enforcing a 1073741824-byte footprint limit";
/// A limit above the launch estimate on any Mac, so the run owes the fit line too: 1 TiB, which
/// the banner spells as 2^40 bytes.
const FIT_LIMIT: &str = "1TiB";
const FIT_LIMIT_BYTES: u64 = 1 << 40;
const FIT_LIMIT_IN_BANNER: &str = "enforcing a 1099511627776-byte footprint limit";
/// The interposer's variable, which the supervised command must not inherit.
const INSERT_ENV: &str = "DYLD_INSERT_LIBRARIES";
/// Set by the test for the interposer: the file it creates once it has held a call.
const MARKER_ENV: &str = "EARLY_EXIT_BANNER_HOLD_MARKER";
/// Read by builds that trace the notice writer; ignored by builds without that trace.
const NOTICE_TRACE_ENV: &str = "MLX_GUARD_NOTICE_TRACE";
/// `/bin/echo` exits in milliseconds; a supervisor still running after ten seconds is stuck.
const RUN_DEADLINE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(10);

/// The `getpgid` interposer. The hold applies once, and only to a pid that is this process's own
/// child (`waitid` returns `ECHILD` at once for any other pid), so later calls run unchanged.
const HOLD_SOURCE: &str = r#"#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int held = 0;

static pid_t hold_getpgid(pid_t pid) {
    if (!held && pid > 0) {
        siginfo_t info;
        int rc;
        do {
            rc = waitid(P_PID, (id_t)pid, &info, WEXITED | WNOWAIT);
        } while (rc == -1 && errno == EINTR);
        if (rc == 0) {
            held = 1;
            const char *marker = getenv("EARLY_EXIT_BANNER_HOLD_MARKER");
            if (marker != NULL) {
                int fd = open(marker, O_WRONLY | O_CREAT | O_EXCL, 0600);
                if (fd >= 0) {
                    dprintf(fd, "pid %d code %d status %d\n", (int)pid, info.si_code,
                            info.si_status);
                    close(fd);
                }
            }
        }
    }
    return getpgid(pid);
}

__attribute__((used)) static struct {
    const void *replacement;
    const void *replacee;
} interposers[] __attribute__((section("__DATA,__interpose"))) = {
    {(const void *)hold_getpgid, (const void *)getpgid},
};
"#;

/// A command that prints whether `DYLD_INSERT_LIBRARIES` reached it. It is built into the scratch
/// directory, so unlike a system binary nothing strips the variable from it before it runs.
const ENV_PRINTER_SOURCE: &str = r#"#include <stdio.h>
#include <stdlib.h>

int main(void) {
    const char *value = getenv("DYLD_INSERT_LIBRARIES");
    if (value == NULL) {
        printf("DYLD_INSERT_LIBRARIES unset\n");
    } else {
        printf("DYLD_INSERT_LIBRARIES=%s\n", value);
    }
    return 0;
}
"#;

/// An owner-only scratch directory (the supervisor refuses any other), removed on drop.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-early-exit-banner-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Compile `source_text` with the system `cc` into `directory`, as `name`, passing `flags` first.
fn compile(directory: &Path, name: &str, source_text: &str, flags: &[&str]) -> PathBuf {
    let source = directory.join(format!("{name}.c"));
    let output_path = directory.join(name);
    fs::write(&source, source_text).unwrap();
    let output = Command::new("cc")
        .args(flags)
        .arg("-o")
        .arg(&output_path)
        .arg(&source)
        .output()
        .expect("the system C compiler `cc` must be available to build the test's helpers");
    assert!(
        output.status.success(),
        "{name} must compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output_path
}

/// Compile the interposer into `directory` and return the library's path.
fn build_hold_library(directory: &Path) -> PathBuf {
    compile(directory, "hold.dylib", HOLD_SOURCE, &["-dynamiclib"])
}

/// Wait for the supervisor within `RUN_DEADLINE`, killing it if it is still running then.
fn wait_within_deadline(child: &mut std::process::Child) -> ExitStatus {
    let deadline = Instant::now() + RUN_DEADLINE;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            panic!("the supervisor was still running after {RUN_DEADLINE:?}");
        }
        thread::sleep(POLL);
    }
}

/// What one `mlx-guard run` left behind.
struct Run {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    /// The interposer's marker, when the hold engaged.
    held: Option<String>,
    report: Option<ReportV1>,
    /// Everything above, printed by every failure message.
    observed: String,
}

/// The command most runs supervise: it prints `STDOUT_TOKEN` and exits in milliseconds.
const ECHO: [&str; 2] = ["/bin/echo", STDOUT_TOKEN];

/// Run `mlx-guard run --max-footprint <limit> -- <child>` with its files under `scratch`, named
/// after `name`. With `interposer`, the supervisor runs with that library loaded.
fn run_guard(
    scratch: &Path,
    name: &str,
    limit: &str,
    child: &[&str],
    interposer: Option<&Path>,
) -> Run {
    let marker = scratch.join(format!("{name}-hold-marker"));
    let trace = scratch.join(format!("{name}-notice-trace"));
    let report_path = scratch.join(format!("{name}-report.json"));
    let stdout_path = scratch.join(format!("{name}-stdout"));
    let stderr_path = scratch.join(format!("{name}-stderr"));

    let mut command = Command::new(GUARD);
    command
        .args(["run", "--max-footprint", limit, "--clear-env", "--report"])
        .arg(&report_path)
        .arg("--")
        .args(child)
        .env(MARKER_ENV, &marker)
        .env(NOTICE_TRACE_ENV, &trace)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout_path).unwrap())
        .stderr(fs::File::create(&stderr_path).unwrap());
    if let Some(library) = interposer {
        command.env(INSERT_ENV, library);
    }
    let mut supervisor = command.spawn().expect("the supervisor must start");
    let status = wait_within_deadline(&mut supervisor);

    let stdout = fs::read_to_string(&stdout_path).unwrap();
    let stderr = fs::read_to_string(&stderr_path).unwrap();
    let held = fs::read_to_string(&marker).ok();
    let trace_text =
        fs::read_to_string(&trace).unwrap_or_else(|error| format!("unreadable: {error}"));
    let report = fs::read_to_string(&report_path)
        .ok()
        .and_then(|text| ReportV1::from_json(&text).ok());
    let observed = format!(
        "{name} run:\nexit {:?}\nstdout {stdout:?}\nstderr {stderr:?}\nhold marker {held:?}\n\
         notice trace: {trace_text}\nreport outcome {:?}\nreport samples {:?}",
        status.code(),
        report.as_ref().map(|report| &report.outcome),
        report.as_ref().map(|report| report.samples.len()),
    );
    Run {
        status,
        stdout,
        stderr,
        held,
        report,
        observed,
    }
}

/// A run under the interposer, after checking that the hold engaged: the command had exited
/// (`CLD_EXITED` is 1) with status 0 before the supervisor's first look at it. Without this, a
/// pass could be the ordinary path.
fn held_run(scratch: &Path, name: &str, limit: &str, child: &[&str]) -> Run {
    let library = build_hold_library(scratch);
    let run = run_guard(scratch, name, limit, child, Some(&library));
    let held = run
        .held
        .as_deref()
        .unwrap_or_else(|| panic!("the hold never engaged:\n{}", run.observed));
    assert!(
        held.ends_with(" code 1 status 0\n"),
        "the command must have exited with status 0 before the first inspection: {held:?}\n{}",
        run.observed
    );
    run
}

#[test]
fn a_command_gone_before_the_first_inspection_still_gets_the_launch_banner() {
    // Catches the launch banner being sent only after the pre-supervision steps succeed: a command
    // that exits before identity inspection (or checkpoint negotiation) then takes the early
    // return and its run ends with exit 0 and an empty stderr, the banner never sent.
    let scratch = TestDirectory::new();
    let run = held_run(&scratch.0, "held", LIMIT, &ECHO);
    let observed = &run.observed;

    // The command's own result is kept: its exit code, its stdout and its report outcome.
    assert_eq!(run.status.code(), Some(0), "{observed}");
    assert!(
        run.stdout.starts_with(&format!("{STDOUT_TOKEN}\n")),
        "the command's stdout must come through unchanged:\n{observed}"
    );
    let report = run
        .report
        .as_ref()
        .unwrap_or_else(|| panic!("the run must write its report:\n{observed}"));
    // The run took the early exit: the hold engaged on the command itself, not on some other
    // child of the supervisor while the command went on to be sampled.
    assert_eq!(
        report.samples.len(),
        0,
        "the held run must end before its first sample:\n{observed}"
    );
    assert_eq!(
        report.outcome.kind,
        TerminalKind::ChildExited { code: 0 },
        "{observed}"
    );
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 0 }),
        "{observed}"
    );
    // The command was launched, so the run announces the ceiling it enforces.
    assert!(
        run.stderr
            .lines()
            .next()
            .is_some_and(|line| line.contains(BANNER_WORDS)),
        "a launched command must get the launch banner on stderr even when it exits before the \
         supervisor inspects it:\n{observed}"
    );
}

#[test]
fn the_early_exit_banner_is_the_ordinary_banner_and_the_report_still_shows_the_early_exit() {
    // Catches the early-exit path printing a different banner from the ordinary one for the same
    // limit (another wording, a second banner, a missing newline, a diagnostic line after it), and
    // the fix changing what the early exit records: the report must keep no samples, the child's
    // own outcome at 0 ms with no footprint, and stdout the same one-line summary as before.
    let scratch = TestDirectory::new();
    // The oracle's command outlives the supervisor's first look at it by a wide margin, so the
    // oracle takes the ordinary path even on a starved machine.
    let outlives_inspection = format!("/bin/echo {STDOUT_TOKEN}; /bin/sleep 1");
    let ordinary = run_guard(
        &scratch.0,
        "ordinary",
        LIMIT,
        &["/bin/sh", "-c", &outlives_inspection],
        None,
    );
    let early = held_run(&scratch.0, "early", LIMIT, &ECHO);
    let observed = format!("{}\n\n{}", ordinary.observed, early.observed);

    // The oracle: the ordinary run's stderr is its banner for this limit, and only that.
    assert!(
        ordinary
            .report
            .as_ref()
            .is_some_and(|report| !report.samples.is_empty()),
        "the ordinary run must reach sampling, or both sides are early exits:\n{observed}"
    );
    assert!(
        ordinary.held.is_none(),
        "the ordinary run must not load the interposer:\n{observed}"
    );
    assert_eq!(ordinary.status.code(), Some(0), "{observed}");
    let ordinary_lines: Vec<&str> = ordinary.stderr.lines().collect();
    assert_eq!(
        ordinary_lines.len(),
        1,
        "a 1 GiB run's stderr must be the banner alone:\n{observed}"
    );
    assert!(
        ordinary_lines[0].contains(LIMIT_IN_BANNER) && ordinary_lines[0].contains(BANNER_WORDS),
        "the ordinary banner must name the 1 GiB limit and the emergency KILL:\n{observed}"
    );

    // The early exit prints the same bytes on stderr: the banner, once, with nothing after it.
    assert_eq!(early.stderr, ordinary.stderr, "{observed}");

    // And it is still an early exit, recorded as before the change.
    assert_eq!(early.status.code(), Some(0), "{observed}");
    assert_eq!(
        early.stdout,
        format!("{STDOUT_TOKEN}\nmlx-guard: child_exited at 0ms; 0 samples, 0 signals\n"),
        "{observed}"
    );
    let report = early
        .report
        .as_ref()
        .unwrap_or_else(|| panic!("the early exit must write its report:\n{observed}"));
    assert_eq!(report.samples.len(), 0, "{observed}");
    assert_eq!(report.outcome.at_ms, 0, "{observed}");
    assert_eq!(
        report.outcome.kind,
        TerminalKind::ChildExited { code: 0 },
        "{observed}"
    );
    assert_eq!(
        report.outcome.child_status,
        Some(ChildStatus::Exited { code: 0 }),
        "{observed}"
    );
    assert_eq!(
        report.outcome.final_footprint_bytes,
        Observed::Unknown,
        "{observed}"
    );
}

#[test]
fn a_command_gone_before_the_first_inspection_gets_the_fit_line_after_the_banner() {
    // Catches the fit line being sent only once the pre-supervision steps succeed while the
    // banner moved ahead of them: a held run above the launch estimate then prints the banner
    // alone, or the two lines in the wrong order.
    let scratch = TestDirectory::new();
    let run = held_run(&scratch.0, "held-fit", FIT_LIMIT, &ECHO);
    let observed = &run.observed;

    assert_eq!(run.status.code(), Some(0), "{observed}");
    let report = run
        .report
        .as_ref()
        .unwrap_or_else(|| panic!("the run must write its report:\n{observed}"));
    assert_eq!(report.samples.len(), 0, "{observed}");
    let Some(Observed::Available { value: estimate }) = report
        .prelaunch
        .as_ref()
        .map(|prelaunch| prelaunch.memory_estimate.clone())
    else {
        panic!("the report must hold the launch estimate the fit line compares with:\n{observed}");
    };
    let (banner, after_banner) = run
        .stderr
        .split_once('\n')
        .unwrap_or_else(|| panic!("stderr must start with a whole banner line:\n{observed}"));
    assert!(
        banner.contains(BANNER_WORDS) && banner.contains(FIT_LIMIT_IN_BANNER),
        "the first stderr line must be the banner for the 1 TiB limit:\n{observed}"
    );
    assert_eq!(
        after_banner,
        fit_line(FIT_LIMIT_BYTES, estimate, mlx_guard_core::VERSION),
        "the fit line must follow the banner, once, with nothing after it:\n{observed}"
    );
}

#[test]
fn the_supervised_command_does_not_inherit_the_interposer() {
    // Catches the interposer's variable reaching the command: where dyld does not strip it, a
    // system binary such as `/bin/echo` (arm64e) is aborted for loading the arm64 library, and
    // the run fails with the command's signal 6 instead of its exit 0.
    let scratch = TestDirectory::new();
    let printer = compile(&scratch.0, "print-insert-env", ENV_PRINTER_SOURCE, &[]);
    let printer = printer.to_str().expect("the scratch path is UTF-8");
    let run = held_run(&scratch.0, "held-env", LIMIT, &[printer]);
    let observed = &run.observed;

    assert_eq!(run.status.code(), Some(0), "{observed}");
    assert!(
        run.stdout.starts_with(&format!("{INSERT_ENV} unset\n")),
        "the command must start without the supervisor's {INSERT_ENV}:\n{observed}"
    );
    let report = run
        .report
        .as_ref()
        .unwrap_or_else(|| panic!("the run must write its report:\n{observed}"));
    assert_eq!(report.samples.len(), 0, "{observed}");
}
