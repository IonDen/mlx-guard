#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use mlx_guard_core::{
    FootprintSampler, IdentityTracker, LaunchOptions, NativeProcessInventory, OwnedProcess,
    SamplingConfig, StdioMode,
};
use serde::Serialize;

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");

/// Path to the `mlx-guard` CLI binary, resolved as a sibling of this crate's fixture binary in the
/// cargo target directory. `CARGO_BIN_EXE_mlx-guard` is not defined here — that variable exists
/// only for the crate that declares the binary — so the real-binary soak requires the CLI binary
/// to be built first (the reference-host soak script builds it) and fails clearly if it is not.
fn mlx_guard_binary() -> std::path::PathBuf {
    let path = std::path::Path::new(FIXTURE)
        .parent()
        .expect("fixture binary has a parent directory")
        .join("mlx-guard");
    assert!(
        path.exists(),
        "the mlx-guard CLI binary is not built at {}; build it with \
         `cargo build -p mlx-guard-cli --bin mlx-guard` before running the real-binary soak",
        path.display()
    );
    path
}

/// 10 ms sampling with a 256-sample ring, matching the churn endurance test in
/// `footprint_sampling.rs`. A short window keeps every escapee observable across several samples.
fn soak_sampler_for(process: &OwnedProcess, inventory: NativeProcessInventory) -> FootprintSampler {
    let root = inventory
        .inspect(process.root_pid().cast_signed())
        .unwrap()
        .identity;
    let tracker = IdentityTracker::new(root, process.process_group_id()).unwrap();
    let config = SamplingConfig::new(
        Duration::from_millis(10),
        Duration::from_millis(100),
        Duration::from_millis(10),
        256,
    )
    .unwrap();
    FootprintSampler::new(config, tracker)
}

fn fixture(mode: &str, bytes: u64, wall_ms: u64) -> LaunchOptions {
    LaunchOptions {
        command: vec![
            OsString::from(FIXTURE),
            OsString::from(mode),
            OsString::from(bytes.to_string()),
            OsString::from(wall_ms.to_string()),
        ],
        cwd: None,
        clear_env: false,
        env: BTreeMap::new(),
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    }
}

fn read_phase(output: &mut BufReader<std::process::ChildStdout>, expected: &str) {
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert!(
        line.starts_with(expected),
        "expected {expected:?}, got {line:?}"
    );
}

#[test]
fn setsid_churn_accumulates_distinct_escapes_with_the_evidence_cap_held() {
    // Catches a churn mode that never actually escapes (children keeping the owned group) or that
    // reuses one identity: distinct escapees must climb past the 64-entry evidence cap while the
    // retained evidence set stays capped. Fast, non-ignored smoke coverage for the setsid-churn
    // fixture mode; the long soaks drive the same setsid-stall escapees from a shell loop. The
    // deadline leaves the 3 vCPU CI runner several times the reference host's clearing time.
    let inventory = NativeProcessInventory::new();
    let mut process = OwnedProcess::launch(&fixture("setsid-churn", 6, 9_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(&mut output, "READY mode=setsid-churn");
    let mut sampler = soak_sampler_for(&process, inventory);
    let epoch = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline && sampler.escaped_count() < 80 {
        sampler.sample_native(&inventory, epoch);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        sampler.escaped_count() >= 80,
        "distinct escapes never reached the floor: escaped_count={}",
        sampler.escaped_count()
    );
    assert_eq!(
        sampler.escaped_evidence_len(),
        64,
        "retained evidence must stay capped under churn"
    );
    process.kill_group().unwrap();
    let _ = process.wait_root();
}

const SOAK_DEFAULT_SECONDS: u64 = 30;
const SOAK_REFERENCE_SECONDS: u64 = 1_800;
const SOAK_MAX_SECONDS: u64 = 3_600;

/// Wall time each escapee lives before it exits and is replaced. Five sample intervals at the
/// soak's 10 ms sampling, so every escapee is observed before it exits; a child that leaves the
/// owned group and its parent link faster than one interval is legitimately never counted
/// (`docs/IDENTITY_AND_CONTAINMENT.md`), so the soak asserts a floor, never an exact census.
const CHURN_ESCAPEE_HOLD_MS: u64 = 50;

/// A loose gross-leak ceiling on the in-process sampler's own resident size, matching the
/// non-escaping churn endurance test's 20 MiB bound. The tight, measured supervisor RSS-delta
/// ceiling lives on the real-binary soak variant, whose footprint is the binary's own and is not
/// contaminated by this harness's per-sample bookkeeping. Measured max resident here was ~3.3 MiB
/// over 60 s at 6 spawners; the 64-entry evidence cap has its own direct, RSS-noise-free assertion
/// below and in the `identity_tracking` unit test.
const SOAK_MAX_RSS_BYTES: u64 = 20 * 1024 * 1024;
/// Measured 60 s CPU was ~2.4% of one core at 10 ms sampling; the ceiling leaves margin while
/// staying far below the 0051 regression's ~25% (rebuilding the escape list on every sample).
const SOAK_MAX_CPU_PERCENT: f64 = 5.0;
/// Measured escape rate was ~98/s (6 spawners): 60 s cleared ~5,800 and 1800 s clears ~175,000.
/// The floor sits far above both the 64-entry evidence cap and any short-run noise.
const SOAK_MIN_DISTINCT_ESCAPES: u64 = 3_000;
/// The published reference sample-window bound; measured p95 here was well under 400 us.
const SOAK_MAX_P95_WINDOW: Duration = Duration::from_millis(10);
/// The real `mlx-guard` binary's own footprint-delta ceiling over the run. The 4.4 GB incident was
/// the binary, not the in-process sampler, so this variant reads the binary's own footprint.
/// Measured unmutated after a 60 s warm-up: at six spawners (~100 escapes/s) 64 KiB over 60 s and
/// 144 KiB over 300 s (34,571 escapes), flat from 180 s to the end of that run; at two spawners
/// 112 KiB over 1800 s, flat from 480 s. The 1800 s six-spawner reference run confirms or corrects
/// the plateau. Growth arrives in 16 KiB page steps and plateaus, so the ceiling leaves
/// several times that margin while staying below what unbounded per-pid retention costs at
/// reference length: dropping the 64-entry evidence cap retained ~36 bytes per escape, 2.08 MiB
/// over 1800 s at two spawners.
const SOAK_MAX_BINARY_RSS_DELTA_BYTES: u64 = 1024 * 1024;
/// The real binary's own CPU share over the run: measured 3.1 % of one core over 300 s at six
/// spawners and 3.4 % over 60 s at two (debug build, 10 ms sampling). The ceiling leaves margin
/// for a loaded host while staying far below the ~25 % the per-sample escape-list rebuild cost
/// before it was removed.
const SOAK_MAX_BINARY_CPU_PERCENT: f64 = 8.0;
/// Spawners driving the real binary: the same six the in-process soak uses, ~100 escapes/s. A
/// tracked child's exit used to cost one unusable sample, which capped this at two spawners (three
/// could produce three consecutive exits at 10 ms and fail observation closed); an exit is now a
/// containment event, and six spawners ran 300 s to SIGTERM with no unusable sample in the final
/// 4,096-window ring. The JSON's `unusable_samples` (full-run calibration count) and
/// `longest_unusable_streak_in_final_window` fields show how close a run sat to that edge.
/// `MLX_GUARD_SOAK_SPAWNERS` overrides it for experiments.
const SOAK_BINARY_DEFAULT_SPAWNERS: usize = 6;
/// Distinct-escape floor for the real binary, stated as measured rate x duration (design rule):
/// six spawners measured ~100 escapes/s and two ~33/s; a tenth of the six-spawner rate survives a
/// loaded host and still puts an 1800 s run at 18,600 — far above the 64-entry cap.
const SOAK_BINARY_MIN_ESCAPES_PER_SECOND: u64 = 10;

/// Build the `/bin/sh -c` program that drives the escaping churn: `spawners` parallel subloops,
/// each serially launching short-lived `mlx-guard-fixture setsid-stall` escapees, bounded by an
/// iteration budget derived once from the lifetime (no `date` fork). Shared by the in-process
/// group and the real-binary variant so both drive identical churn.
fn escaping_churn_program(spawners: usize, lifetime: Duration) -> String {
    assert!((1..=8).contains(&spawners));
    let iterations = lifetime.as_millis() / u128::from(CHURN_ESCAPEE_HOLD_MS) + 64;
    let subloop = format!(
        "i=0; while [ \"$i\" -lt {iterations} ]; do '{FIXTURE}' setsid-stall 1 \
         {CHURN_ESCAPEE_HOLD_MS} >/dev/null 2>&1; i=$((i+1)); done"
    );
    let mut program = String::new();
    for _ in 0..spawners {
        program.push('(');
        program.push_str(&subloop);
        program.push_str(") & ");
    }
    program.push_str("wait");
    program
}

/// A long-lived escaping-churn workload the in-process sampler can supervise for the full soak.
///
/// The synthetic fixture is hard-capped at ten seconds (`MAX_FIXTURE_WALL_TIME`), so — exactly as
/// the non-escaping endurance test uses a raw `/bin/sh` `ChurnProcessGroup` rather than the
/// fixture — the long-lived root here is `/bin/sh`. It runs `spawners` parallel subloops, each
/// serially launching short-lived `mlx-guard-fixture setsid-stall` escapees. Each escapee opens
/// its own session (leaves the owned group) and self-exits at its ~50 ms watchdog, so distinct
/// escaped identities accumulate at the aggregate spawn rate. The loop carries no `date` fork; a
/// once-computed iteration budget bounds a leaked group's lifetime, and `Drop` kills the group.
struct EscapingChurnGroup {
    process_group_id: i32,
    child: Child,
}

impl EscapingChurnGroup {
    fn spawn(spawners: usize, lifetime: Duration) -> Self {
        let program = escaping_churn_program(spawners, lifetime);
        let child = Command::new("/bin/sh")
            .args(["-c", &program])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let process_group_id = child.id().cast_signed();
        Self {
            process_group_id,
            child,
        }
    }

    fn root_pid(&self) -> i32 {
        self.child.id().cast_signed()
    }
}

impl Drop for EscapingChurnGroup {
    fn drop(&mut self) {
        // SAFETY: this harness created and retained the positive process group until this cleanup.
        // The escapees have left the group by design, so a handful may outlive this kill by up to
        // their ~50 ms watchdog; they self-terminate and are not left running.
        unsafe {
            libc::kill(-self.process_group_id, libc::SIGKILL);
        }
        self.child.wait().ok();
    }
}

fn cpu_seconds() -> f64 {
    let mut value = std::mem::MaybeUninit::<libc::timespec>::zeroed();
    // SAFETY: clock_gettime writes the full timespec for the current-process CPU clock on success.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, value.as_mut_ptr()) },
        0
    );
    // SAFETY: the successful clock_gettime call initialized the output structure.
    let value = unsafe { value.assume_init() };
    Duration::new(
        value.tv_sec.cast_unsigned(),
        u32::try_from(value.tv_nsec).unwrap(),
    )
    .as_secs_f64()
}

fn maximum_resident_bytes() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage writes the complete rusage value for this process on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    // SAFETY: the successful getrusage call initialized the output structure.
    let usage = unsafe { usage.assume_init() };
    u64::try_from(usage.ru_maxrss).unwrap()
}

fn current_footprint_bytes(inventory: NativeProcessInventory) -> u64 {
    inventory
        .inspect(std::process::id().cast_signed())
        .unwrap()
        .footprint_bytes
        .unwrap()
}

#[derive(Serialize)]
struct SoakObservation {
    elapsed_milliseconds: u64,
    distinct_escapes: u64,
    escaped_evidence_len: usize,
    current_footprint_bytes: u64,
    maximum_resident_bytes: u64,
    history_length: usize,
}

#[derive(Serialize)]
struct SoakResult {
    schema_version: u16,
    requested_duration_seconds: u64,
    actual_duration_nanoseconds: u64,
    sample_interval_milliseconds: u64,
    spawners: usize,
    total_samples: u64,
    distinct_escapes: u64,
    escaped_evidence_len: usize,
    cpu_seconds: f64,
    cpu_percent_of_one_core: f64,
    starting_footprint_bytes: u64,
    final_footprint_bytes: u64,
    footprint_growth_bytes: u64,
    maximum_resident_bytes: u64,
    p95_window_nanoseconds: u64,
    final_history_length: usize,
    raw_observations: Vec<SoakObservation>,
    tight_bounds_applied: bool,
    reference_run: bool,
}

fn write_soak_output(result: &SoakResult) {
    let Some(path) = std::env::var_os("MLX_GUARD_SOAK_OUTPUT") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
}

fn assert_soak_bounds(result: &SoakResult) {
    // Invariants that hold at any duration on the reference host: the escape-evidence cap and the
    // sample-history ring stay bounded regardless of how many distinct pids churn through, the
    // per-sample window stays within the published bound, and a gross resident-size leak is caught
    // even in-process. The tight, measured supervisor RSS-delta ceiling is asserted on the
    // real-binary variant, whose footprint is not contaminated by this harness's own bookkeeping.
    assert!(
        result.total_samples >= 10,
        "too few samples: {}",
        result.total_samples
    );
    assert!(
        result.distinct_escapes > 0,
        "no escapes observed under churn"
    );
    assert!(
        result.escaped_evidence_len <= 64,
        "evidence cap breached: {}",
        result.escaped_evidence_len
    );
    if result.tight_bounds_applied {
        // A tracker that never retains evidence at all would also stay under the cap.
        assert_eq!(
            result.escaped_evidence_len, 64,
            "evidence list did not fill to the cap under sustained churn"
        );
    }
    assert!(
        result.final_history_length <= 256,
        "history_len={}",
        result.final_history_length
    );
    let p95 = Duration::from_nanos(result.p95_window_nanoseconds);
    assert!(
        p95 <= SOAK_MAX_P95_WINDOW,
        "p95 sample window too wide: {p95:?}"
    );
    assert!(
        result.maximum_resident_bytes <= SOAK_MAX_RSS_BYTES,
        "max_resident={} exceeds ceiling {SOAK_MAX_RSS_BYTES}",
        result.maximum_resident_bytes
    );

    // The distinct-escape floor and the CPU ceiling apply on measurement-grade runs (>= 60 s),
    // whose longer warmup and sample count make them meaningful; the reference bundle run is 1800 s.
    if result.tight_bounds_applied {
        assert!(
            result.distinct_escapes >= SOAK_MIN_DISTINCT_ESCAPES,
            "distinct_escapes below floor: {} < {SOAK_MIN_DISTINCT_ESCAPES}",
            result.distinct_escapes
        );
        assert!(
            result.cpu_percent_of_one_core <= SOAK_MAX_CPU_PERCENT,
            "cpu_percent={:.4} exceeds ceiling {SOAK_MAX_CPU_PERCENT}",
            result.cpu_percent_of_one_core
        );
    }
}

#[test]
#[ignore = "reference-host escaping-churn soak acceptance"]
// A single measurement procedure: warmup, the sampling loop with periodic observations, and the
// summary record. Splitting it would scatter the run's state across helpers without making the
// soak clearer; the assertions are already extracted into assert_soak_bounds.
#[allow(clippy::too_many_lines)]
fn escaping_churn_supervisor_footprint_stays_bounded() {
    let run_seconds = std::env::var("MLX_GUARD_SOAK_SECONDS")
        .map_or(SOAK_DEFAULT_SECONDS, |value| value.parse::<u64>().unwrap());
    assert!((1..=SOAK_MAX_SECONDS).contains(&run_seconds));
    let spawners =
        std::env::var("MLX_GUARD_SOAK_SPAWNERS").map_or(6, |value| value.parse::<usize>().unwrap());
    // A longer warmup on measurement-grade runs captures the baseline after the footprint has
    // ramped, so the recorded delta is genuine over-run growth, not cold-start allocation.
    let warmup = if run_seconds >= 60 {
        Duration::from_secs(10)
    } else {
        Duration::from_millis(500)
    };
    let duration = Duration::from_secs(run_seconds);
    let group = EscapingChurnGroup::spawn(spawners, warmup + duration + Duration::from_secs(10));
    thread::sleep(warmup);

    let inventory = NativeProcessInventory::new();
    let root = inventory.inspect(group.root_pid()).unwrap().identity;
    let tracker = IdentityTracker::new(root, group.process_group_id).unwrap();
    let config = SamplingConfig::new(
        Duration::from_millis(10),
        Duration::from_millis(100),
        Duration::from_millis(10),
        256,
    )
    .unwrap();
    let mut sampler = FootprintSampler::new(config, tracker);
    let epoch = Instant::now();

    let started = Instant::now();
    let cpu_started = cpu_seconds();
    let footprint_started = current_footprint_bytes(inventory);
    let mut max_resident = maximum_resident_bytes();
    let mut total_samples = 0_u64;
    // Pre-sized so the harness's own per-sample bookkeeping does not reallocate mid-run and show
    // up in the footprint it measures (a doubling at 131,072 entries once cost 2 MiB of "growth").
    let mut windows =
        Vec::with_capacity(usize::try_from(duration.as_millis() / 10).unwrap() + 4_096);
    let mut observations = vec![SoakObservation {
        elapsed_milliseconds: 0,
        distinct_escapes: 0,
        escaped_evidence_len: 0,
        current_footprint_bytes: footprint_started,
        maximum_resident_bytes: max_resident,
        history_length: sampler.history_len(),
    }];
    let mut next_progress = Duration::from_secs(30);

    while started.elapsed() < duration {
        let sample = sampler.sample_native(&inventory, epoch);
        total_samples += 1;
        windows.push(sample.finished_at.checked_sub(sample.started_at).unwrap());
        if started.elapsed() >= next_progress {
            let footprint = current_footprint_bytes(inventory);
            max_resident = max_resident.max(maximum_resident_bytes());
            observations.push(SoakObservation {
                elapsed_milliseconds: u64::try_from(started.elapsed().as_millis()).unwrap(),
                distinct_escapes: sampler.escaped_count(),
                escaped_evidence_len: sampler.escaped_evidence_len(),
                current_footprint_bytes: footprint,
                maximum_resident_bytes: max_resident,
                history_length: sampler.history_len(),
            });
            eprintln!(
                "soak elapsed_s={} distinct_escapes={} evidence_len={} footprint={footprint} max_rss={max_resident} history={}",
                started.elapsed().as_secs(),
                sampler.escaped_count(),
                sampler.escaped_evidence_len(),
                sampler.history_len(),
            );
            next_progress += Duration::from_secs(30);
        }
        thread::sleep(sampler.delay_until_next(epoch.elapsed()).unwrap());
    }

    let elapsed = started.elapsed();
    let cpu_used = cpu_seconds() - cpu_started;
    let final_footprint = current_footprint_bytes(inventory);
    max_resident = max_resident.max(maximum_resident_bytes());
    let cpu_percent = cpu_used / elapsed.as_secs_f64() * 100.0;
    let distinct_escapes = sampler.escaped_count();
    let evidence_len = sampler.escaped_evidence_len();

    windows.sort_unstable();
    let p95_index = (windows.len() * 95 / 100).min(windows.len().saturating_sub(1));
    let p95 = windows[p95_index];
    let rss_delta = final_footprint.saturating_sub(footprint_started);

    observations.push(SoakObservation {
        elapsed_milliseconds: u64::try_from(elapsed.as_millis()).unwrap(),
        distinct_escapes,
        escaped_evidence_len: evidence_len,
        current_footprint_bytes: final_footprint,
        maximum_resident_bytes: max_resident,
        history_length: sampler.history_len(),
    });

    eprintln!(
        "soak complete elapsed_s={} spawners={spawners} distinct_escapes={distinct_escapes} evidence_len={evidence_len} cpu_percent={cpu_percent:.4} start_footprint={footprint_started} final_footprint={final_footprint} rss_delta={rss_delta} max_rss={max_resident} p95_window={p95:?} total_samples={total_samples}",
        elapsed.as_secs()
    );

    let result = SoakResult {
        schema_version: 1,
        requested_duration_seconds: run_seconds,
        actual_duration_nanoseconds: u64::try_from(elapsed.as_nanos()).unwrap(),
        sample_interval_milliseconds: 10,
        spawners,
        total_samples,
        distinct_escapes,
        escaped_evidence_len: evidence_len,
        cpu_seconds: cpu_used,
        cpu_percent_of_one_core: cpu_percent,
        starting_footprint_bytes: footprint_started,
        final_footprint_bytes: final_footprint,
        footprint_growth_bytes: rss_delta,
        maximum_resident_bytes: max_resident,
        p95_window_nanoseconds: u64::try_from(p95.as_nanos()).unwrap(),
        final_history_length: sampler.history_len(),
        raw_observations: observations,
        tight_bounds_applied: run_seconds >= 60,
        reference_run: run_seconds == SOAK_REFERENCE_SECONDS,
    };
    write_soak_output(&result);
    assert_soak_bounds(&result);
}

/// Count the samples the policy could not use and the longest run of them, from the per-sample
/// "aggregate available" flags in report order. The supervisor fails observation closed at three
/// consecutive unusable samples, so the longest streak says how close a run sat to that edge.
fn unusable_sample_summary(available: impl IntoIterator<Item = bool>) -> (u64, u64) {
    let mut unusable = 0_u64;
    let mut streak = 0_u64;
    let mut longest = 0_u64;
    for usable in available {
        if usable {
            streak = 0;
        } else {
            unusable += 1;
            streak += 1;
            longest = longest.max(streak);
        }
    }
    (unusable, longest)
}

/// CPU seconds (user + system) consumed so far by another process, read through
/// `proc_pid_rusage` and converted from Mach absolute-time units.
fn process_cpu_seconds(pid: i32) -> Option<f64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v0>::zeroed();
    // SAFETY: the V0 buffer is writable and proc_pid_rusage fills the whole structure on success.
    let status = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V0,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: the successful call initialized the structure.
    let info = unsafe { info.assume_init() };
    let mut timebase = MachTimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: mach_timebase_info writes the two fields of the provided structure and returns
    // KERN_SUCCESS (0) on every supported host.
    let status = unsafe { mach_timebase_info(&raw mut timebase) };
    if status != 0 || timebase.denom == 0 {
        return None;
    }
    let ticks = info.ri_user_time.saturating_add(info.ri_system_time);
    #[allow(clippy::cast_precision_loss)]
    let nanos = ticks as f64 * f64::from(timebase.numer) / f64::from(timebase.denom);
    Some(nanos / 1e9)
}

/// `mach_timebase_info_data_t`: the numerator/denominator that converts Mach absolute-time ticks
/// to nanoseconds (125/3 on Apple Silicon). Declared here because the `libc` binding is deprecated
/// in favour of a crate this workspace does not pin.
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::c_int;
}

#[test]
fn unusable_sample_summary_counts_and_finds_the_longest_streak() {
    // Catches a summary that counts but never tracks the streak (or resets it wrongly): the
    // sequence below has four unusable samples whose longest run is two, not four and not one.
    let flags = [true, false, false, true, false, true, true, false];
    assert_eq!(unusable_sample_summary(flags), (4, 2));
    assert_eq!(unusable_sample_summary([true, true]), (0, 0));
    assert_eq!(unusable_sample_summary([false, false, false]), (3, 3));
}

#[derive(Serialize)]
struct BinarySoakObservation {
    elapsed_milliseconds: u64,
    footprint_bytes: u64,
    cpu_seconds: f64,
}

#[derive(Serialize)]
struct BinarySoakResult {
    schema_version: u16,
    requested_duration_seconds: u64,
    warmup_seconds: u64,
    sample_interval_milliseconds: u64,
    spawners: usize,
    baseline_footprint_bytes: u64,
    peak_footprint_bytes: u64,
    footprint_delta_bytes: u64,
    cpu_seconds: f64,
    cpu_percent_of_one_core: f64,
    escaped_count: u64,
    total_samples: u64,
    unusable_samples: u64,
    final_window_samples: u64,
    longest_unusable_streak_in_final_window: u64,
    outcome_kind: String,
    ran_to_terminal_signal: bool,
    raw_observations: Vec<BinarySoakObservation>,
    tight_bounds_applied: bool,
    reference_run: bool,
}

fn write_binary_soak_output(result: &BinarySoakResult) {
    let Some(path) = std::env::var_os("MLX_GUARD_SOAK_BINARY_OUTPUT") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
}

#[test]
#[ignore = "reference-host real-binary escaping-churn soak acceptance"]
#[allow(clippy::too_many_lines)]
fn real_binary_supervising_escaping_churn_stays_bounded() {
    let run_seconds = std::env::var("MLX_GUARD_SOAK_SECONDS")
        .map_or(SOAK_DEFAULT_SECONDS, |value| value.parse::<u64>().unwrap());
    assert!((1..=SOAK_MAX_SECONDS).contains(&run_seconds));
    let spawners = std::env::var("MLX_GUARD_SOAK_SPAWNERS")
        .map_or(SOAK_BINARY_DEFAULT_SPAWNERS, |value| {
            value.parse::<usize>().unwrap()
        });
    // Tight-bounds runs take the baseline only after the binary's steady state is reached: the
    // 4,096-sample report ring fills in 41 s at 10 ms sampling (a 30 s warm-up still measured
    // ~900 KiB of ring fill as "growth"), and the allocator settles inside that window too.
    let warmup = if run_seconds >= 60 {
        Duration::from_secs(60)
    } else {
        Duration::from_millis(500)
    };
    let duration = Duration::from_secs(run_seconds);

    // The CLI requires the report's parent directory to already exist, be user-owned, and be 0700.
    let base = std::env::temp_dir().join(format!("mlx-guard-binary-soak-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let report_path = base.join("soak.json");

    // The real supervisor observes a long-lived escaping churn. Observe never intervenes, so the
    // binary's own footprint reflects sustained sampling of many distinct escaped pids — the value
    // the 0051 incident grew without bound — rather than any workload memory spike.
    let program = escaping_churn_program(spawners, warmup + duration + Duration::from_secs(10));
    let mut guard = Command::new(mlx_guard_binary())
        .args(["observe", "--sample-interval", "10ms", "--report"])
        .arg(&report_path)
        .args(["--", "/bin/sh", "-c", &program])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let guard_pid = i32::try_from(guard.id()).unwrap();

    thread::sleep(warmup);
    let inventory = NativeProcessInventory::new();
    let baseline = inventory
        .inspect(guard_pid)
        .ok()
        .and_then(|observation| observation.footprint_bytes)
        .unwrap();
    let cpu_baseline = process_cpu_seconds(guard_pid).unwrap();
    let mut peak = baseline;
    let mut cpu_latest = cpu_baseline;
    let started = Instant::now();
    let mut next_progress = Duration::from_secs(30);
    let mut raw_observations = vec![BinarySoakObservation {
        elapsed_milliseconds: 0,
        footprint_bytes: baseline,
        cpu_seconds: 0.0,
    }];
    let mut exited_early = false;
    while started.elapsed() < duration {
        // A supervisor that dies mid-run (gave up observation, crashed) ends the measurement now
        // rather than after the full window; the outcome assertion below names the failure.
        if guard.try_wait().unwrap().is_some() {
            exited_early = true;
            break;
        }
        if let Ok(observation) = inventory.inspect(guard_pid)
            && let Some(footprint) = observation.footprint_bytes
        {
            peak = peak.max(footprint);
        }
        if let Some(cpu) = process_cpu_seconds(guard_pid) {
            cpu_latest = cpu;
        }
        if started.elapsed() >= next_progress {
            raw_observations.push(BinarySoakObservation {
                elapsed_milliseconds: u64::try_from(started.elapsed().as_millis()).unwrap(),
                footprint_bytes: peak,
                cpu_seconds: cpu_latest - cpu_baseline,
            });
            eprintln!(
                "binary soak elapsed_s={} baseline={baseline} peak={peak} delta={} cpu_s={:.2}",
                started.elapsed().as_secs(),
                peak.saturating_sub(baseline),
                cpu_latest - cpu_baseline
            );
            next_progress += Duration::from_secs(30);
        }
        thread::sleep(Duration::from_millis(200));
    }
    let measured = started.elapsed();
    if let Some(cpu) = process_cpu_seconds(guard_pid) {
        cpu_latest = cpu;
    }

    // End the observe run: the native parent forwards the terminal signal to the owned group and
    // writes the final report before it exits.
    if !exited_early {
        // SAFETY: guard_pid is this test's direct child, launched above and not yet reaped.
        unsafe {
            libc::kill(guard_pid, libc::SIGTERM);
        }
    }
    let _ = guard.wait();

    let report_text = std::fs::read_to_string(&report_path).unwrap();
    // Keep the schema-v1 report for diagnosis when a location is given; the temporary directory
    // is removed below whatever the verdict.
    if let Some(copy) = std::env::var_os("MLX_GUARD_SOAK_BINARY_REPORT") {
        std::fs::write(copy, &report_text).unwrap();
    }
    std::fs::remove_dir_all(&base).ok();
    let report = mlx_guard_core::ReportV1::from_json(&report_text).unwrap();
    let ran_to_terminal_signal =
        report.outcome.kind == mlx_guard_core::TerminalKind::ChildSignaled { signal: 15 };
    let escaped_count = report.escape.escaped_count.unwrap_or(0);
    // The report's sample ring keeps only the last 4,096 windows (41 s at 10 ms); the calibration
    // section counts every sample of the run, so the totals come from there and only the streak
    // is read from the ring.
    let calibration = report
        .calibration
        .as_ref()
        .expect("observe reports carry calibration");
    let total_samples = calibration.total_samples;
    let unusable_samples = calibration.incomplete_samples;
    let final_window_samples = u64::try_from(report.samples.len()).unwrap();
    let (_, longest_unusable_streak) =
        unusable_sample_summary(report.samples.iter().map(|sample| {
            matches!(
                sample.aggregate_footprint_bytes,
                mlx_guard_core::Observed::Available { .. }
            )
        }));
    let delta = peak.saturating_sub(baseline);
    let cpu_used = cpu_latest - cpu_baseline;
    let cpu_percent = cpu_used / measured.as_secs_f64() * 100.0;
    let bounds_applied = run_seconds >= 60;

    eprintln!(
        "binary soak complete elapsed_s={} spawners={spawners} baseline={baseline} peak={peak} \
         delta={delta} cpu_percent={cpu_percent:.3} escaped_count={escaped_count} \
         samples={total_samples} unusable={unusable_samples} \
         final_window_streak={longest_unusable_streak} \
         outcome={:?}",
        measured.as_secs(),
        report.outcome.kind
    );

    write_binary_soak_output(&BinarySoakResult {
        schema_version: 2,
        requested_duration_seconds: run_seconds,
        warmup_seconds: warmup.as_secs(),
        sample_interval_milliseconds: 10,
        spawners,
        baseline_footprint_bytes: baseline,
        peak_footprint_bytes: peak,
        footprint_delta_bytes: delta,
        cpu_seconds: cpu_used,
        cpu_percent_of_one_core: cpu_percent,
        escaped_count,
        total_samples,
        unusable_samples,
        final_window_samples,
        longest_unusable_streak_in_final_window: longest_unusable_streak,
        outcome_kind: format!("{:?}", report.outcome.kind),
        ran_to_terminal_signal,
        raw_observations,
        tight_bounds_applied: bounds_applied,
        reference_run: run_seconds == SOAK_REFERENCE_SECONDS,
    });

    // The run must have ended on this test's SIGTERM. A supervisor that gave up on observation
    // first (three consecutive missing samples under churn the binary cannot follow) exits 70 and
    // relinquishes the churn, and its footprint over a truncated run proves nothing.
    assert!(
        ran_to_terminal_signal,
        "the binary did not run to the test's SIGTERM (spawners={spawners}, measured {}s of {}s); \
         outcome={:?}",
        measured.as_secs(),
        duration.as_secs(),
        report.outcome.kind
    );
    assert!(
        escaped_count > 0,
        "the binary counted no escapes under churn"
    );
    if bounds_applied {
        // The report counts escapes from launch, so the floor spans warm-up plus the window.
        let floor = SOAK_BINARY_MIN_ESCAPES_PER_SECOND * (run_seconds + warmup.as_secs());
        assert!(
            escaped_count >= floor,
            "binary escaped_count below floor: {escaped_count} < {floor}"
        );
        assert!(
            delta <= SOAK_MAX_BINARY_RSS_DELTA_BYTES,
            "binary footprint delta={delta} exceeds ceiling {SOAK_MAX_BINARY_RSS_DELTA_BYTES}"
        );
        assert!(
            cpu_percent <= SOAK_MAX_BINARY_CPU_PERCENT,
            "binary cpu_percent={cpu_percent:.3} exceeds ceiling {SOAK_MAX_BINARY_CPU_PERCENT}"
        );
    }
}
