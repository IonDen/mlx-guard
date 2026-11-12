//! The sample taken after the command exited but before it was reaped, through the real binary.
//!
//! An exiting process stays visible for a few milliseconds after the kernel marks it exited, and
//! a sample that lands in that window measures no process at all. These runs hold a large
//! footprint until the command exits on its own, twenty times over in each mode. Whether a run
//! hits that window depends on the host's timing, so every test prints how many runs did.
//!
//! - `run`: each run that hits the window shows whether the runtime read that sample as a
//!   recovery or a failure, so the two `run` tests pin that call site whenever the window is hit.
//! - `observe`: the test pins that the run keeps its own exit code and records no transition, and
//!   its count shows whether the exit sample reached the report.
//!
//! With `MLX_GUARD_REQUIRE_EXIT_RACE=1`, every test also fails when no run hit the window, because
//! a pass with no hit proves nothing about that sample. The reference host runs these tests with
//! it set: `MLX_GUARD_REQUIRE_EXIT_RACE=1 cargo test -p mlx-guard-cli --test exit_sample`. CI and
//! the release build run without it, so a missed race there is reported, not failed.
//!
//! The tests of a mode read the same twenty runs, so the binary pays for them once.
#![cfg(target_os = "macos")]

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_core::{Observed, PolicyState, ReportV1};

const GUARD: &str = env!("CARGO_BIN_EXE_mlx-guard");
const CLEARED: &str = "mlx-guard: warning cleared: ";
/// `--max-footprint 140MiB`: warning 126 MiB, recovery 112 MiB, emergency 154 MiB. The allocate
/// fixture holds 128 MiB (its own ceiling) plus a base of about 1.2 MiB, which sits in the band
/// and short of the limit.
const LIMIT: &str = "140MiB";
const ALLOCATION: &str = "134217728";
const ALLOCATION_BYTES: u64 = 134_217_728;
const WARNING_BYTES: u64 = 132_120_576;
const LIMIT_BYTES: u64 = 146_800_640;
/// The fixture's wall-time watchdog ends each run with exit code 124 while the allocation is
/// still mapped, so the memory goes only with the process. Closing stdin or sending `exit`
/// would unmap it first, a real recovery these tests must not see. The allocation reaches the
/// band about 25 ms into a run on an M1 Max, well inside the hold.
const WALL_MS: &str = "200";
const FIXTURE_EXIT: i32 = 124;
/// The exit window is not hit on every run, and how often it is hit depends on the host, so each
/// mode repeats the run twenty times. Forty runs keep the binary near 12 s on the reference host.
const RUNS: usize = 20;
/// Set to `1` to require at least one hit of the exit window in each mode.
const REQUIRE_EXIT_RACE: &str = "MLX_GUARD_REQUIRE_EXIT_RACE";
/// One run ends at the fixture's 200 ms watchdog; a supervisor still running at 5 s is stuck.
const RUN_DEADLINE: Duration = Duration::from_secs(5);

/// Held while a mode makes its runs, so no more than one 128 MiB fixture is alive at once even
/// when the test harness runs the two modes' tests on parallel threads.
static ONE_FIXTURE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// An owner-only scratch directory (the supervisor refuses any other), removed on drop.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(mode: Mode) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-exit-sample-{}-{}-{timestamp}",
            mode.subcommand(),
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

/// The two ways the binary supervises a command, each with its own sampling loop.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// `mlx-guard run --max-footprint 140MiB`: the allocation sits in the warning band.
    Enforce,
    /// `mlx-guard observe`: no limit, no band, no intervention.
    Observe,
}

impl Mode {
    const fn subcommand(self) -> &'static str {
        match self {
            Self::Enforce => "run",
            Self::Observe => "observe",
        }
    }
}

/// What one allocate-then-exit run left behind.
struct ExitRun {
    code: Option<i32>,
    stderr: String,
    report: ReportV1,
}

impl ExitRun {
    /// Whether the report holds the unmeasured exit sample, which it records as an available 0.
    /// A live process never reads 0 B, so this marks a run that hit the exit window.
    fn raced(&self) -> bool {
        self.report
            .samples
            .iter()
            .any(|window| window.aggregate_footprint_bytes == Observed::Available { value: 0 })
    }
}

/// One supervised run: allocate 128 MiB, then hold until the fixture's watchdog exits.
fn allocate_then_exit(directory: &Path, mode: Mode, run: usize) -> ExitRun {
    let report_path = directory.join(format!("report-{run}.json"));
    let stderr_path = directory.join(format!("stderr-{run}.txt"));
    let mut command = Command::new(GUARD);
    command.arg(mode.subcommand());
    if matches!(mode, Mode::Enforce) {
        command.args(["--max-footprint", LIMIT]);
    }
    let mut child = command
        .args(["--sample-interval", "10ms"])
        .arg("--report")
        .arg(&report_path)
        .arg("--")
        .arg(fixture())
        .args(["allocate", ALLOCATION, WALL_MS])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(File::create(&stderr_path).unwrap())
        .spawn()
        .expect("the supervisor must start");
    // Held open until the supervisor is done: end of file would make the fixture return and
    // unmap before it exits.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(b"allocate\n").unwrap();
    let deadline = Instant::now() + RUN_DEADLINE;
    let status = loop {
        if let Some(status) = child.try_wait().expect("the supervisor must be waited for") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            panic!("{mode:?} run {run}: the supervisor was still running after {RUN_DEADLINE:?}");
        }
        thread::sleep(Duration::from_millis(5));
    };
    drop(stdin);
    let stderr = fs::read_to_string(&stderr_path).unwrap();
    let report = ReportV1::from_json(&fs::read_to_string(&report_path).unwrap_or_else(|error| {
        panic!("{mode:?} run {run}: no final report ({error}); stderr: {stderr}")
    }))
    .expect("the final report must validate");

    // Preconditions: the run held its memory (in the band, when enforcing). A failure here is
    // the machine, not the bug these tests look for.
    match mode {
        Mode::Enforce => {
            let entered: Vec<_> = report
                .transitions
                .iter()
                .filter(|record| {
                    record.from == PolicyState::Normal && record.to == PolicyState::Warning
                })
                .collect();
            assert_eq!(
                entered.len(),
                1,
                "run {run} must enter the band once: {:?}; stderr: {stderr}",
                report.transitions
            );
            let footprint = entered[0]
                .aggregate_footprint_bytes
                .expect("the crossing record carries its sample");
            assert!(
                (WARNING_BYTES..LIMIT_BYTES).contains(&footprint),
                "run {run}: the crossing must sit in the band: {footprint}"
            );
        }
        Mode::Observe => assert!(
            report.samples.iter().any(|window| matches!(
                window.aggregate_footprint_bytes,
                Observed::Available { value } if value >= ALLOCATION_BYTES
            )),
            "observe run {run} must measure the 128 MiB allocation: {:?}; stderr: {stderr}",
            report.samples
        ),
    }
    ExitRun {
        code: status.code(),
        stderr,
        report,
    }
}

/// Twenty runs of one mode, made one at a time.
fn make_runs(mode: Mode) -> Vec<ExitRun> {
    let _one_fixture = ONE_FIXTURE_AT_A_TIME
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let directory = TestDirectory::new(mode);
    (0..RUNS)
        .map(|run| allocate_then_exit(&directory.0, mode, run))
        .collect()
}

/// The enforcing runs both `run` tests read, made once per test binary.
fn enforce_runs() -> &'static [ExitRun] {
    static RUNS_DONE: OnceLock<Vec<ExitRun>> = OnceLock::new();
    RUNS_DONE.get_or_init(|| make_runs(Mode::Enforce))
}

/// The observing runs, made once per test binary.
fn observe_runs() -> &'static [ExitRun] {
    static RUNS_DONE: OnceLock<Vec<ExitRun>> = OnceLock::new();
    RUNS_DONE.get_or_init(|| make_runs(Mode::Observe))
}

fn raced_count(runs: &[ExitRun]) -> usize {
    runs.iter().filter(|run| run.raced()).count()
}

/// Whether the value of `MLX_GUARD_REQUIRE_EXIT_RACE` asks for at least one hit: exactly `1`.
fn exit_race_required(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

#[test]
fn only_a_value_of_one_requires_the_exit_window() {
    // Bug: the gate reads any value, or the variable's mere presence, as "required" (an empty or
    // `0` setting would then fail CI on a missed race), or it never requires a hit at all (the
    // reference host would stop pinning the `run` call site).
    assert!(exit_race_required(Some(OsStr::new("1"))));
    assert!(!exit_race_required(None));
    assert!(!exit_race_required(Some(OsStr::new("0"))));
    assert!(!exit_race_required(Some(OsStr::new(""))));
}

/// Prints how many runs hit the exit window and, when `MLX_GUARD_REQUIRE_EXIT_RACE=1`, fails if
/// none did. Without the variable a green result says nothing about that sample when the count is
/// 0, so read it before trusting one.
fn require_the_window_where_asked(runs: &[ExitRun], mode: Mode) {
    let raced = raced_count(runs);
    eprintln!("{mode:?}: {raced} of {RUNS} runs hit the exit window");
    if exit_race_required(std::env::var_os(REQUIRE_EXIT_RACE).as_deref()) {
        assert!(
            raced >= 1,
            "none of the {RUNS} {mode:?} runs left the exit sample (an available 0 B) in its \
             report, so none of them tested it ({REQUIRE_EXIT_RACE}=1)"
        );
    }
}

#[test]
fn a_command_that_exits_in_the_band_never_reads_as_a_recovery() {
    // Bug: the sample taken after the command exited but before it was reaped measures nobody,
    // sums to 0 B, and that 0 B clears the warning: a "warning cleared ... 0 B" line and a
    // warning -> normal record at 0 B for a footprint that never fell (the skip removed, or the
    // policy and band fed above it in `run`). Every run that hits the window shows it; with
    // `MLX_GUARD_REQUIRE_EXIT_RACE=1` at least one must hit it.
    let runs = enforce_runs();
    let wrong: Vec<String> = runs
        .iter()
        .enumerate()
        .filter_map(|(run, result)| {
            let cleared: Vec<&str> = result
                .stderr
                .lines()
                .filter(|line| line.starts_with(CLEARED))
                .collect();
            let recovered_at_zero = result.report.transitions.iter().any(|record| {
                record.from == PolicyState::Warning
                    && record.to == PolicyState::Normal
                    && record.aggregate_footprint_bytes == Some(0)
            });
            (!cleared.is_empty() || recovered_at_zero).then(|| {
                format!(
                    "run {run}: cleared lines {cleared:?}, transitions {:?}",
                    result.report.transitions
                )
            })
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {RUNS} runs read the command's exit as a recovery ({} hit the exit window):\n{}",
        wrong.len(),
        raced_count(runs),
        wrong.join("\n")
    );
    require_the_window_where_asked(runs, Mode::Enforce);
}

#[test]
fn a_command_that_exits_in_the_band_ends_with_its_own_exit_code() {
    // Bug: the sample taken after the command exited but before it was reaped fails the run
    // closed (supervisor error, TERM, exit 70) although the command ended on its own: counted
    // as a missing sample when that makes the streak reach the limit, or read as a fault.
    // Reach, measured: at 128 MiB and 10 ms the exit window holds one sample, so a plain
    // "counted as missing" with the limit of three stays green here; the runtime's unit test
    // of the gate pins that case. This run catches it once one exit sample is enough.
    let runs = enforce_runs();
    let wrong: Vec<String> = runs
        .iter()
        .enumerate()
        .filter_map(|(run, result)| {
            let failed_closed = result
                .report
                .transitions
                .iter()
                .any(|record| record.to == PolicyState::SupervisorError);
            (result.code != Some(FIXTURE_EXIT) || failed_closed).then(|| {
                format!(
                    "run {run}: exit {:?}, transitions {:?}, stderr {:?}",
                    result.code, result.report.transitions, result.stderr
                )
            })
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {RUNS} runs did not end with the command's own exit code {FIXTURE_EXIT} ({} hit \
         the exit window):\n{}",
        wrong.len(),
        raced_count(runs),
        wrong.join("\n")
    );
    require_the_window_where_asked(runs, Mode::Enforce);
}

#[test]
fn an_observed_command_that_exits_keeps_its_exit_code_and_records_no_transition() {
    // Bug: `observe` fails the run closed on the sample that measured nobody (a supervisor-error
    // record, not the command's own exit code). Observe never moves between policy states on a
    // reading, so a report that records any transition here, a warning -> normal one at 0 B
    // included, is wrong. With `MLX_GUARD_REQUIRE_EXIT_RACE=1` it also catches a skip placed
    // above the report write (no run leaves the exit sample behind); without the variable that
    // shows only as a count of 0, which is also what a host that misses the window prints.
    let runs = observe_runs();
    let wrong: Vec<String> = runs
        .iter()
        .enumerate()
        .filter(|(_, result)| {
            result.code != Some(FIXTURE_EXIT) || !result.report.transitions.is_empty()
        })
        .map(|(run, result)| {
            format!(
                "run {run}: exit {:?}, transitions {:?}, stderr {:?}",
                result.code, result.report.transitions, result.stderr
            )
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {RUNS} observed runs did not end quietly with the command's own exit code \
         {FIXTURE_EXIT} ({} hit the exit window):\n{}",
        wrong.len(),
        raced_count(runs),
        wrong.join("\n")
    );
    require_the_window_where_asked(runs, Mode::Observe);
}
