#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
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
    // fixture the reference soak drives at length.
    let inventory = NativeProcessInventory::new();
    let mut process = OwnedProcess::launch(&fixture("setsid-churn", 6, 4_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(&mut output, "READY mode=setsid-churn");
    let mut sampler = soak_sampler_for(&process, inventory);
    let epoch = Instant::now();
    let deadline = Instant::now() + Duration::from_millis(3_500);
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
    let mut windows = Vec::new();
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
