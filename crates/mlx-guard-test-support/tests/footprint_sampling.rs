#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use mlx_guard_core::{
    FootprintSample, FootprintSampler, IdentityTracker, LaunchOptions, NativeProcessInventory,
    OwnedProcess, RootOutcome, SampleOutcome, SamplingConfig, StdioMode,
};
use serde::Serialize;

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");
const ALLOCATION_BYTES: u64 = 64 * 1024 * 1024;
const ENDURANCE_MEMBER_COUNT: usize = 16;
const ENDURANCE_HISTORY_CAPACITY: usize = 256;
const MAX_RSS_BYTES: u64 = 20 * 1024 * 1024;
const MAX_FOOTPRINT_GROWTH_BYTES: u64 = 2 * 1024 * 1024;

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
        stdin: StdioMode::Piped,
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

fn send(input: &mut std::process::ChildStdin, command: &str) {
    writeln!(input, "{command}").unwrap();
    input.flush().unwrap();
}

fn complete_bytes(sample: &FootprintSample) -> u64 {
    match sample.outcome {
        SampleOutcome::Complete { total_bytes } => total_bytes,
        ref outcome => panic!("expected complete footprint, got {outcome:?}"),
    }
}

fn sampler_for(process: &OwnedProcess, inventory: NativeProcessInventory) -> FootprintSampler {
    let root = inventory
        .inspect(process.root_pid().cast_signed())
        .unwrap()
        .identity;
    let tracker = IdentityTracker::new(root, process.process_group_id()).unwrap();
    let config = SamplingConfig::new(
        Duration::from_millis(10),
        Duration::from_millis(100),
        Duration::from_millis(10),
        32,
    )
    .unwrap();
    FootprintSampler::new(config, tracker)
}

#[test]
fn anonymous_allocation_and_release_match_the_predeclared_reference_bounds() {
    // Catches substituting RSS, mapping size, or delayed synthetic values for Darwin footprint.
    let inventory = NativeProcessInventory::new();
    inventory.probe_footprint().unwrap();
    let mut process = OwnedProcess::launch(&fixture("allocate", ALLOCATION_BYTES, 5_000)).unwrap();
    let mut input = process.take_stdin().unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(&mut output, "READY mode=allocate");
    let mut sampler = sampler_for(&process, inventory);
    let epoch = Instant::now();
    let baseline = complete_bytes(sampler.sample_native(&inventory, epoch));

    send(&mut input, "allocate");
    read_phase(&mut output, "ALLOCATED");
    let allocated = sampler.sample_native(&inventory, epoch).clone();
    let allocated_delta = complete_bytes(&allocated).saturating_sub(baseline);
    let magnitude_error = allocated_delta.abs_diff(ALLOCATION_BYTES);
    assert!(
        magnitude_error <= ALLOCATION_BYTES / 100,
        "baseline={baseline} allocated_delta={allocated_delta} magnitude_error={magnitude_error} sample={allocated:#?}"
    );
    assert!(
        allocated
            .finished_at
            .checked_sub(allocated.started_at)
            .unwrap()
            <= Duration::from_millis(10)
    );

    send(&mut input, "release");
    read_phase(&mut output, "RELEASED");
    let released = sampler.sample_native(&inventory, epoch).clone();
    let release_residual = complete_bytes(&released).saturating_sub(baseline);
    assert!(
        release_residual <= ALLOCATION_BYTES / 100,
        "baseline={baseline} release_residual={release_residual} sample={released:#?}"
    );
    assert!(
        released
            .finished_at
            .checked_sub(released.started_at)
            .unwrap()
            <= Duration::from_millis(10)
    );

    send(&mut input, "exit");
    read_phase(&mut output, "EXIT");
    assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(0));
}

#[test]
fn shared_mapping_calibration_keeps_both_mapping_owners_alive_while_sampling() {
    // Catches measuring a shared mapping only after the forked helper has already exited.
    let inventory = NativeProcessInventory::new();
    let mut process =
        OwnedProcess::launch(&fixture("shared-live", ALLOCATION_BYTES, 5_000)).unwrap();
    let mut input = process.take_stdin().unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(&mut output, "READY mode=shared-live");
    let mut sampler = sampler_for(&process, inventory);
    let epoch = Instant::now();

    send(&mut input, "allocate");
    read_phase(&mut output, "ALLOCATED");
    let allocated = sampler.sample_native(&inventory, epoch).clone();
    assert_eq!(
        allocated.members.len(),
        2,
        "shared aggregation requires two live owners: {allocated:#?}"
    );
    assert!(
        allocated
            .members
            .iter()
            .all(|member| member.footprint_bytes.is_some()),
        "both live owners must have an OS-accounted footprint: {allocated:#?}"
    );

    send(&mut input, "release");
    read_phase(&mut output, "RELEASED");
    let released = sampler.sample_native(&inventory, epoch).clone();
    assert_eq!(released.members.len(), 1, "helper must exit on release");

    send(&mut input, "exit");
    read_phase(&mut output, "EXIT");
    assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(0));
}

#[test]
fn bounded_ramp_exposes_incremental_growth_instead_of_one_immediate_allocation() {
    // Catches a ramp fixture that allocates its full ceiling before the supervisor can sample it.
    let inventory = NativeProcessInventory::new();
    let mut process = OwnedProcess::launch(&fixture("ramp", 128 * 1024 * 1024, 3_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(
        &mut output,
        "READY mode=ramp rate_bytes_per_second=134217728",
    );
    let mut sampler = sampler_for(&process, inventory);
    let epoch = Instant::now();
    let baseline = complete_bytes(sampler.sample_native(&inventory, epoch));

    thread::sleep(Duration::from_millis(180));
    let observed = complete_bytes(sampler.sample_native(&inventory, epoch));
    let growth = observed.saturating_sub(baseline);
    assert!(
        (4 * 1024 * 1024..=32 * 1024 * 1024).contains(&growth),
        "expected bounded incremental growth after 80ms of ramping, got {growth} bytes"
    );

    process.kill_group().unwrap();
    let _ = process.wait_root().unwrap();
}

#[test]
fn sampling_continues_after_term_while_a_worker_is_still_alive() {
    // Catches stopping observation at signal delivery instead of at observed process exit.
    let inventory = NativeProcessInventory::new();
    let mut process = OwnedProcess::launch(&fixture("ignore-term", 1, 2_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(&mut output, "READY mode=ignore-term");
    let mut sampler = sampler_for(&process, inventory);
    let epoch = Instant::now();

    process.terminate_group().unwrap();
    let after_term = sampler.sample_native(&inventory, epoch).clone();
    assert!(after_term.members.iter().any(|member| {
        member.identity.pid == process.root_pid().cast_signed() && member.footprint_bytes.is_some()
    }));
    process.kill_group().unwrap();
    process.wait_root().unwrap();
}

#[test]
fn targeted_sampling_still_detects_a_child_that_leaves_the_owned_group() {
    // Catches optimizing group enumeration by dropping the separate descendant walk.
    let inventory = NativeProcessInventory::new();
    let mut process = OwnedProcess::launch(&fixture("setsid-parent", 1, 2_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(&mut output, "ESCAPED");
    let mut sampler = sampler_for(&process, inventory);
    let sample = sampler.sample_native(&inventory, Instant::now()).clone();
    assert!(sample.escape_observed);
    assert!(
        sample
            .members
            .iter()
            .any(|member| { member.identity.pid == process.root_pid().cast_signed() })
    );
}

#[test]
fn targeted_sampling_observes_real_child_churn_without_growing_history() {
    // Catches a group fast path that only ever returns the root process.
    let inventory = NativeProcessInventory::new();
    let mut process = OwnedProcess::launch(&fixture("spawn-churn", 1, 2_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(&mut output, "READY mode=spawn-churn");
    let mut sampler = sampler_for(&process, inventory);
    let epoch = Instant::now();
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut saw_child = false;
    while Instant::now() < deadline && process.try_wait_root().unwrap().is_none() {
        let sample = sampler.sample_native(&inventory, epoch);
        saw_child |= sample.members.len() > 1;
        thread::sleep(Duration::from_millis(2));
    }
    assert!(saw_child);
    assert!(sampler.history_len() <= sampler.history_capacity());
}

#[test]
fn sixteen_member_group_sample_window_p95_stays_within_ten_milliseconds() {
    // Catches performing unbounded work or serial waits inside one observation window.
    let inventory = NativeProcessInventory::new();
    let mut process = OwnedProcess::launch(&fixture("fanout-stall", 16, 3_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(&mut output, "READY mode=fanout-stall members=16");
    let mut sampler = sampler_for(&process, inventory);
    let epoch = Instant::now();
    let mut windows = Vec::new();
    for _ in 0..50 {
        let sample = sampler.sample_native(&inventory, epoch);
        assert_eq!(sample.members.len(), 16);
        windows.push(sample.finished_at.checked_sub(sample.started_at).unwrap());
    }
    windows.sort_unstable();
    let p95 = windows[47];
    eprintln!("sixteen_member_sample_window_p95={p95:?}");
    assert!(p95 <= Duration::from_millis(10), "p95={p95:?}");
}

struct IdleProcessGroup {
    process_group_id: i32,
    children: Vec<Child>,
}

impl IdleProcessGroup {
    fn spawn(member_count: usize, lifetime: Duration) -> Self {
        assert!((1..=16).contains(&member_count));
        let lifetime_seconds = lifetime.as_secs().max(1).to_string();
        let root = Command::new("/bin/sleep")
            .arg(&lifetime_seconds)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let process_group_id = root.id().cast_signed();
        let mut group = Self {
            process_group_id,
            children: vec![root],
        };
        for _ in 1..member_count {
            let child = Command::new("/bin/sleep")
                .arg(&lifetime_seconds)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(process_group_id)
                .spawn()
                .unwrap();
            group.children.push(child);
        }
        group
    }

    fn root_pid(&self) -> i32 {
        self.children[0].id().cast_signed()
    }
}

impl Drop for IdleProcessGroup {
    fn drop(&mut self) {
        // SAFETY: this harness created and retained the positive process group until this cleanup.
        unsafe {
            libc::kill(-self.process_group_id, libc::SIGKILL);
        }
        for child in &mut self.children {
            child.wait().ok();
        }
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

fn current_footprint_bytes(inventory: NativeProcessInventory) -> u64 {
    inventory
        .inspect(std::process::id().cast_signed())
        .unwrap()
        .footprint_bytes
        .unwrap()
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

#[derive(Serialize)]
struct EnduranceObservation {
    elapsed_milliseconds: u64,
    current_footprint_bytes: u64,
    maximum_resident_bytes: u64,
    history_length: usize,
}

#[derive(Serialize)]
struct EnduranceResult {
    schema_version: u16,
    requested_duration_seconds: u64,
    actual_duration_nanoseconds: u64,
    sample_interval_milliseconds: u64,
    member_count: usize,
    history_capacity: usize,
    cpu_seconds: f64,
    cpu_percent_of_one_core: f64,
    starting_footprint_bytes: u64,
    final_footprint_bytes: u64,
    footprint_growth_bytes: u64,
    maximum_resident_bytes: u64,
    final_history_length: usize,
    raw_observations: Vec<EnduranceObservation>,
    full_bounds_applied: bool,
}

#[allow(clippy::too_many_arguments)]
fn write_endurance_output(
    run_seconds: u64,
    elapsed: Duration,
    cpu_used: f64,
    cpu_percent: f64,
    footprint_started: u64,
    final_footprint: u64,
    max_resident: u64,
    history_length: usize,
    observations: Vec<EnduranceObservation>,
) {
    let Some(path) = std::env::var_os("MLX_GUARD_ENDURANCE_OUTPUT") else {
        return;
    };
    let result = EnduranceResult {
        schema_version: 1,
        requested_duration_seconds: run_seconds,
        actual_duration_nanoseconds: u64::try_from(elapsed.as_nanos()).unwrap(),
        sample_interval_milliseconds: 50,
        member_count: ENDURANCE_MEMBER_COUNT,
        history_capacity: ENDURANCE_HISTORY_CAPACITY,
        cpu_seconds: cpu_used,
        cpu_percent_of_one_core: cpu_percent,
        starting_footprint_bytes: footprint_started,
        final_footprint_bytes: final_footprint,
        footprint_growth_bytes: final_footprint.saturating_sub(footprint_started),
        maximum_resident_bytes: max_resident,
        final_history_length: history_length,
        raw_observations: observations,
        full_bounds_applied: run_seconds == 1_800,
    };
    let path = std::path::PathBuf::from(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
}

fn assert_full_endurance_bounds(
    run_seconds: u64,
    history_length: usize,
    cpu_percent: f64,
    max_resident: u64,
    footprint_started: u64,
    final_footprint: u64,
) {
    if run_seconds != 1_800 {
        assert!(history_length <= ENDURANCE_HISTORY_CAPACITY);
        return;
    }
    assert_eq!(history_length, ENDURANCE_HISTORY_CAPACITY);
    assert!(cpu_percent <= 2.0, "cpu_percent={cpu_percent:.4}");
    assert!(max_resident <= MAX_RSS_BYTES, "max_resident={max_resident}");
    assert!(
        final_footprint <= footprint_started.saturating_add(MAX_FOOTPRINT_GROWTH_BYTES),
        "start_footprint={footprint_started} final_footprint={final_footprint}"
    );
}

fn endurance_warmup(run_seconds: u64) -> Duration {
    if run_seconds >= 60 {
        Duration::from_secs(10)
    } else {
        Duration::from_millis(100)
    }
}

#[test]
#[ignore = "30-minute reference-host endurance acceptance"]
fn thirty_minute_sampler_stays_inside_cpu_rss_and_history_bounds() {
    let run_seconds = std::env::var("MLX_GUARD_ENDURANCE_SECONDS")
        .map_or(1_800, |value| value.parse::<u64>().unwrap());
    assert!((1..=1_800).contains(&run_seconds));
    let warmup = endurance_warmup(run_seconds);
    let duration = Duration::from_secs(run_seconds);
    let group = IdleProcessGroup::spawn(
        ENDURANCE_MEMBER_COUNT,
        warmup + duration + Duration::from_secs(30),
    );
    let inventory = NativeProcessInventory::new();
    let root = inventory.inspect(group.root_pid()).unwrap().identity;
    let tracker = IdentityTracker::new(root, group.process_group_id).unwrap();
    let config = SamplingConfig::new(
        Duration::from_millis(50),
        Duration::from_millis(100),
        Duration::from_millis(10),
        ENDURANCE_HISTORY_CAPACITY,
    )
    .unwrap();
    let mut sampler = FootprintSampler::new(config, tracker);
    let epoch = Instant::now();
    let warmup_deadline = Instant::now() + warmup;
    while Instant::now() < warmup_deadline {
        let sample = sampler.sample_native(&inventory, epoch);
        assert_eq!(sample.members.len(), ENDURANCE_MEMBER_COUNT);
        thread::sleep(sampler.delay_until_next(epoch.elapsed()).unwrap());
    }

    let started = Instant::now();
    let cpu_started = cpu_seconds();
    let footprint_started = current_footprint_bytes(inventory);
    let mut max_resident = maximum_resident_bytes();
    let mut observations = vec![EnduranceObservation {
        elapsed_milliseconds: 0,
        current_footprint_bytes: footprint_started,
        maximum_resident_bytes: max_resident,
        history_length: sampler.history_len(),
    }];
    let mut next_progress = Duration::from_secs(60);
    while started.elapsed() < duration {
        let sample = sampler.sample_native(&inventory, epoch);
        assert_eq!(sample.members.len(), ENDURANCE_MEMBER_COUNT);
        assert!(matches!(sample.outcome, SampleOutcome::Complete { .. }));
        if started.elapsed() >= next_progress {
            let footprint = current_footprint_bytes(inventory);
            max_resident = max_resident.max(maximum_resident_bytes());
            observations.push(EnduranceObservation {
                elapsed_milliseconds: u64::try_from(started.elapsed().as_millis()).unwrap(),
                current_footprint_bytes: footprint,
                maximum_resident_bytes: max_resident,
                history_length: sampler.history_len(),
            });
            eprintln!(
                "endurance elapsed_s={} footprint_bytes={} max_resident_bytes={} history_len={}",
                started.elapsed().as_secs(),
                footprint,
                max_resident,
                sampler.history_len()
            );
            next_progress += Duration::from_secs(60);
        }
        thread::sleep(sampler.delay_until_next(epoch.elapsed()).unwrap());
    }
    let elapsed = started.elapsed();
    let cpu_used = cpu_seconds() - cpu_started;
    let final_footprint = current_footprint_bytes(inventory);
    max_resident = max_resident.max(maximum_resident_bytes());
    let cpu_percent = cpu_used / elapsed.as_secs_f64() * 100.0;
    observations.push(EnduranceObservation {
        elapsed_milliseconds: u64::try_from(elapsed.as_millis()).unwrap(),
        current_footprint_bytes: final_footprint,
        maximum_resident_bytes: max_resident,
        history_length: sampler.history_len(),
    });
    eprintln!(
        "endurance complete elapsed_s={} cpu_percent={cpu_percent:.4} start_footprint_bytes={footprint_started} final_footprint_bytes={final_footprint} max_resident_bytes={max_resident} history_len={}",
        elapsed.as_secs(),
        sampler.history_len()
    );

    write_endurance_output(
        run_seconds,
        elapsed,
        cpu_used,
        cpu_percent,
        footprint_started,
        final_footprint,
        max_resident,
        sampler.history_len(),
        observations,
    );

    assert_full_endurance_bounds(
        run_seconds,
        sampler.history_len(),
        cpu_percent,
        max_resident,
        footprint_started,
        final_footprint,
    );
}

struct ChurnProcessGroup {
    process_group_id: i32,
    child: Child,
}

impl ChurnProcessGroup {
    fn spawn(lifetime: Duration) -> Self {
        let lifetime_seconds = lifetime.as_secs().max(1);
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                &format!(
                    "end=$(($(date +%s) + {lifetime_seconds})); \
                     while [ \"$(date +%s)\" -lt \"$end\" ]; do /bin/sleep 0.05; done"
                ),
            ])
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

impl Drop for ChurnProcessGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.process_group_id, libc::SIGKILL);
        }
        self.child.wait().ok();
    }
}

const CHURN_DEFAULT_SECONDS: u64 = 30;
const CHURN_MAX_SECONDS: u64 = 300;
const CHURN_MAX_RSS_BYTES: u64 = 20 * 1024 * 1024;
const CHURN_MAX_FOOTPRINT_GROWTH_BYTES: u64 = 4 * 1024 * 1024;

#[test]
#[ignore = "bounded pid-churn endurance acceptance"]
fn pid_churn_sampler_rss_stays_bounded_despite_many_distinct_children() {
    let run_seconds = std::env::var("MLX_GUARD_CHURN_SECONDS")
        .map_or(CHURN_DEFAULT_SECONDS, |value| value.parse::<u64>().unwrap());
    assert!((1..=CHURN_MAX_SECONDS).contains(&run_seconds));
    let warmup = Duration::from_millis(500);
    let duration = Duration::from_secs(run_seconds);
    let group = ChurnProcessGroup::spawn(warmup + duration + Duration::from_secs(10));
    thread::sleep(warmup);

    let inventory = NativeProcessInventory::new();
    let root = inventory.inspect(group.root_pid()).unwrap().identity;
    let tracker = IdentityTracker::new(root, group.process_group_id).unwrap();
    let config = SamplingConfig::new(
        Duration::from_millis(50),
        Duration::from_millis(100),
        Duration::from_millis(50),
        ENDURANCE_HISTORY_CAPACITY,
    )
    .unwrap();
    let mut sampler = FootprintSampler::new(config, tracker);
    let epoch = Instant::now();

    let started = Instant::now();
    let cpu_started = cpu_seconds();
    let footprint_started = current_footprint_bytes(inventory);
    let mut max_resident = maximum_resident_bytes();
    let mut total_samples = 0_u64;
    let mut distinct_root_samples = 0_u64;
    let mut next_progress = Duration::from_secs(10);
    let mut windows = Vec::new();

    while started.elapsed() < duration {
        let sample = sampler.sample_native(&inventory, epoch);
        total_samples += 1;
        if sample
            .members
            .iter()
            .any(|member| member.identity.pid == group.root_pid())
        {
            distinct_root_samples += 1;
        }
        windows.push(sample.finished_at.checked_sub(sample.started_at).unwrap());
        if started.elapsed() >= next_progress {
            let footprint = current_footprint_bytes(inventory);
            max_resident = max_resident.max(maximum_resident_bytes());
            eprintln!(
                "churn elapsed_s={} samples={total_samples} footprint_bytes={footprint} max_rss={}",
                started.elapsed().as_secs(),
                max_resident
            );
            next_progress += Duration::from_secs(10);
        }
        thread::sleep(sampler.delay_until_next(epoch.elapsed()).unwrap());
    }

    let elapsed = started.elapsed();
    let cpu_used = cpu_seconds() - cpu_started;
    let final_footprint = current_footprint_bytes(inventory);
    max_resident = max_resident.max(maximum_resident_bytes());
    let cpu_percent = cpu_used / elapsed.as_secs_f64() * 100.0;

    windows.sort_unstable();
    let p95_index = (windows.len() * 95 / 100).min(windows.len().saturating_sub(1));
    let p95 = windows[p95_index];

    eprintln!(
        "churn complete elapsed_s={elapsed_s} total_samples={total_samples} \
         root_samples={distinct_root_samples} cpu_percent={cpu_percent:.4} \
         start_footprint={footprint_started} final_footprint={final_footprint} \
         max_rss={max_resident} p95_window={p95:?}",
        elapsed_s = elapsed.as_secs()
    );

    assert!(total_samples >= 10, "too few samples: {total_samples}");
    assert!(
        distinct_root_samples > 0,
        "root was never observed in samples"
    );
    assert!(
        p95 <= Duration::from_millis(10),
        "p95 sample window too wide: {p95:?}"
    );
    assert!(
        max_resident <= CHURN_MAX_RSS_BYTES,
        "max_resident={max_resident}"
    );
    assert!(
        final_footprint <= footprint_started.saturating_add(CHURN_MAX_FOOTPRINT_GROWTH_BYTES),
        "start={footprint_started} final={final_footprint} growth={}",
        final_footprint.saturating_sub(footprint_started)
    );
    assert!(
        sampler.history_len() <= ENDURANCE_HISTORY_CAPACITY,
        "history_len={}",
        sampler.history_len()
    );
}
