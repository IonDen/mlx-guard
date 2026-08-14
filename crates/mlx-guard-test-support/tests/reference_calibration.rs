#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use mlx_guard_core::{
    FootprintSample, FootprintSampler, IdentityTracker, LaunchOptions, NativeProcessInventory,
    OwnedProcess, RootOutcome, SampleOutcome, SamplingConfig, StdioMode,
};
use serde::Serialize;

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");
const ALLOCATION_BYTES: u64 = 64 * 1024 * 1024;
const SYNTHETIC_CEILING_BYTES: u64 = 128 * 1024 * 1024;
const SYNTHETIC_CEILING_MILLISECONDS: u64 = 10_000;
const REPETITIONS: usize = 10;
const NATIVE_CALL_REPETITIONS: usize = 256;
const GROUP_WINDOW_REPETITIONS: usize = 100;

#[derive(Serialize)]
struct ReferenceCalibration {
    schema_version: u16,
    provenance: Provenance,
    fixture_ceilings: FixtureCeilings,
    proc_pid_rusage: LatencyMeasurements,
    anonymous_mapping: MappingMeasurements,
    live_shared_mapping: MappingMeasurements,
    metal_mapping: MappingMeasurements,
    sixteen_member_sample_window: LatencyMeasurements,
    bounds: Vec<BoundResult>,
}

#[derive(Serialize)]
struct Provenance {
    git_commit: String,
    git_status_porcelain: String,
    captured_at_utc: String,
    hardware: String,
    macos: String,
    kernel_release: String,
    architecture: String,
    rustc: String,
    cargo: String,
    clang: String,
    command: String,
}

#[derive(Serialize)]
struct FixtureCeilings {
    synthetic_allocation_bytes: u64,
    synthetic_wall_milliseconds: u64,
}

#[derive(Serialize)]
struct LatencyMeasurements {
    unit: &'static str,
    raw: Vec<u64>,
    p95: u64,
    maximum: u64,
}

#[derive(Serialize)]
struct MappingMeasurements {
    requested_bytes: u64,
    raw: Vec<MappingSample>,
    maximum_magnitude_error_bytes: u64,
    maximum_release_residual_bytes: u64,
}

#[derive(Serialize)]
struct MappingSample {
    baseline_bytes: u64,
    allocated_bytes: u64,
    released_bytes: u64,
    allocated_delta_bytes: u64,
    magnitude_error_bytes: u64,
    release_residual_bytes: u64,
    allocated_member_count: usize,
    release_observation_attempts: usize,
    release_outcomes: Vec<String>,
    allocation_window_nanoseconds: u64,
    release_window_nanoseconds: u64,
}

#[derive(Serialize)]
struct BoundResult {
    measure: &'static str,
    target: &'static str,
    observed: u64,
    unit: &'static str,
    passed: bool,
}

fn command_output(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
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
        stdin: StdioMode::Piped,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    }
}

fn metal_fixture() -> LaunchOptions {
    let executable = std::env::var_os("MLX_GUARD_METAL_FIXTURE")
        .map(PathBuf::from)
        .expect("MLX_GUARD_METAL_FIXTURE must name the built calibration fixture")
        .canonicalize()
        .unwrap();
    LaunchOptions {
        command: vec![executable.into_os_string()],
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

fn sample_until_complete(
    sampler: &mut FootprintSampler,
    inventory: NativeProcessInventory,
    epoch: Instant,
) -> (FootprintSample, Vec<String>) {
    let mut outcomes = Vec::with_capacity(3);
    for attempt in 1..=3 {
        let sample = sampler.sample_native(&inventory, epoch).clone();
        outcomes.push(format!("{:?}", sample.outcome));
        if matches!(sample.outcome, SampleOutcome::Complete { .. }) {
            assert_eq!(outcomes.len(), attempt);
            return (sample, outcomes);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("footprint did not become complete within three bounded observations");
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

fn nanoseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap()
}

fn latency_measurements(mut raw: Vec<u64>) -> LatencyMeasurements {
    raw.sort_unstable();
    let p95_index = (raw.len() * 95).div_ceil(100) - 1;
    LatencyMeasurements {
        unit: "nanoseconds",
        p95: raw[p95_index],
        maximum: *raw.last().unwrap(),
        raw,
    }
}

fn measure_native_calls(inventory: NativeProcessInventory) -> LatencyMeasurements {
    let pid = std::process::id().cast_signed();
    let mut raw = Vec::with_capacity(NATIVE_CALL_REPETITIONS);
    for _ in 0..NATIVE_CALL_REPETITIONS {
        let started = Instant::now();
        let observation = inventory.inspect(pid).unwrap();
        raw.push(nanoseconds(started.elapsed()));
        assert!(observation.footprint_bytes.is_some());
    }
    latency_measurements(raw)
}

fn measure_mapping(
    inventory: NativeProcessInventory,
    mode: &'static str,
    expected_members: usize,
) -> MappingMeasurements {
    let mut raw = Vec::with_capacity(REPETITIONS);
    for _ in 0..REPETITIONS {
        let mut process = OwnedProcess::launch(&fixture(mode, ALLOCATION_BYTES, 5_000)).unwrap();
        let mut input = process.take_stdin().unwrap();
        let mut output = BufReader::new(process.take_stdout().unwrap());
        read_phase(&mut output, &format!("READY mode={mode}"));
        let mut sampler = sampler_for(&process, inventory);
        let epoch = Instant::now();
        let baseline = complete_bytes(sampler.sample_native(&inventory, epoch));

        send(&mut input, "allocate");
        read_phase(&mut output, "ALLOCATED");
        let allocated = sampler.sample_native(&inventory, epoch).clone();
        let allocated_bytes = complete_bytes(&allocated);
        let allocated_delta = allocated_bytes.saturating_sub(baseline);

        send(&mut input, "release");
        read_phase(&mut output, "RELEASED");
        let (released, release_outcomes) = sample_until_complete(&mut sampler, inventory, epoch);
        let released_bytes = complete_bytes(&released);

        send(&mut input, "exit");
        read_phase(&mut output, "EXIT");
        assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(0));
        assert_eq!(allocated.members.len(), expected_members);

        raw.push(MappingSample {
            baseline_bytes: baseline,
            allocated_bytes,
            released_bytes,
            allocated_delta_bytes: allocated_delta,
            magnitude_error_bytes: allocated_delta.abs_diff(ALLOCATION_BYTES),
            release_residual_bytes: released_bytes.saturating_sub(baseline),
            allocated_member_count: allocated.members.len(),
            release_observation_attempts: release_outcomes.len(),
            release_outcomes,
            allocation_window_nanoseconds: nanoseconds(
                allocated.finished_at.saturating_sub(allocated.started_at),
            ),
            release_window_nanoseconds: nanoseconds(
                released.finished_at.saturating_sub(released.started_at),
            ),
        });
    }
    MappingMeasurements {
        requested_bytes: ALLOCATION_BYTES,
        maximum_magnitude_error_bytes: raw
            .iter()
            .map(|sample| sample.magnitude_error_bytes)
            .max()
            .unwrap(),
        maximum_release_residual_bytes: raw
            .iter()
            .map(|sample| sample.release_residual_bytes)
            .max()
            .unwrap(),
        raw,
    }
}

fn measure_group_windows(inventory: NativeProcessInventory) -> LatencyMeasurements {
    let mut process = OwnedProcess::launch(&fixture("fanout-stall", 16, 5_000)).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(&mut output, "READY mode=fanout-stall members=16");
    let mut sampler = sampler_for(&process, inventory);
    let epoch = Instant::now();
    let mut raw = Vec::with_capacity(GROUP_WINDOW_REPETITIONS);
    for _ in 0..GROUP_WINDOW_REPETITIONS {
        let sample = sampler.sample_native(&inventory, epoch);
        assert_eq!(sample.members.len(), 16);
        raw.push(nanoseconds(
            sample.finished_at.saturating_sub(sample.started_at),
        ));
    }
    process.kill_group().unwrap();
    let _ = process.wait_root().unwrap();
    latency_measurements(raw)
}

fn measure_metal(inventory: NativeProcessInventory) -> MappingMeasurements {
    let mut raw = Vec::with_capacity(REPETITIONS);
    for _ in 0..REPETITIONS {
        let mut process = OwnedProcess::launch(&metal_fixture()).unwrap();
        let mut input = process.take_stdin().unwrap();
        let mut output = BufReader::new(process.take_stdout().unwrap());
        read_phase(&mut output, "READY mode=metal-calibration bytes=67108864");
        let mut sampler = sampler_for(&process, inventory);
        let epoch = Instant::now();
        let baseline = complete_bytes(sampler.sample_native(&inventory, epoch));

        send(&mut input, "allocate");
        read_phase(&mut output, "ALLOCATED");
        let allocated = sampler.sample_native(&inventory, epoch).clone();
        let allocated_bytes = complete_bytes(&allocated);
        let allocated_delta = allocated_bytes.saturating_sub(baseline);

        send(&mut input, "release");
        read_phase(&mut output, "RELEASED");
        let (released, release_outcomes) = sample_until_complete(&mut sampler, inventory, epoch);
        let released_bytes = complete_bytes(&released);

        send(&mut input, "exit");
        read_phase(&mut output, "EXIT");
        assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(0));
        raw.push(MappingSample {
            baseline_bytes: baseline,
            allocated_bytes,
            released_bytes,
            allocated_delta_bytes: allocated_delta,
            magnitude_error_bytes: allocated_delta.abs_diff(ALLOCATION_BYTES),
            release_residual_bytes: released_bytes.saturating_sub(baseline),
            allocated_member_count: allocated.members.len(),
            release_observation_attempts: release_outcomes.len(),
            release_outcomes,
            allocation_window_nanoseconds: nanoseconds(
                allocated.finished_at.saturating_sub(allocated.started_at),
            ),
            release_window_nanoseconds: nanoseconds(
                released.finished_at.saturating_sub(released.started_at),
            ),
        });
    }
    MappingMeasurements {
        requested_bytes: ALLOCATION_BYTES,
        maximum_magnitude_error_bytes: raw
            .iter()
            .map(|sample| sample.magnitude_error_bytes)
            .max()
            .unwrap(),
        maximum_release_residual_bytes: raw
            .iter()
            .map(|sample| sample.release_residual_bytes)
            .max()
            .unwrap(),
        raw,
    }
}

fn sanitized_hardware_details(raw: &str) -> String {
    const ALLOWED_FIELDS: [&str; 5] = [
        "Model Name:",
        "Model Identifier:",
        "Chip:",
        "Total Number of Cores:",
        "Memory:",
    ];
    raw.lines()
        .map(str::trim)
        .filter(|line| ALLOWED_FIELDS.iter().any(|field| line.starts_with(field)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn provenance() -> Provenance {
    let raw_hardware = command_output("system_profiler", &["SPHardwareDataType"]);
    Provenance {
        git_commit: command_output("git", &["rev-parse", "HEAD"]),
        git_status_porcelain: command_output("git", &["status", "--porcelain"]),
        captured_at_utc: command_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        hardware: sanitized_hardware_details(&raw_hardware),
        macos: command_output("sw_vers", &[]),
        kernel_release: command_output("uname", &["-r"]),
        architecture: command_output("uname", &["-m"]),
        rustc: command_output("rustc", &["--version", "--verbose"]),
        cargo: command_output("cargo", &["--version", "--verbose"]),
        clang: command_output("clang", &["--version"]),
        command: "MLX_GUARD_METAL_FIXTURE=<path> MLX_GUARD_CALIBRATION_OUTPUT=<path> cargo test -p mlx-guard-test-support --test reference_calibration reference_host_footprint_measurements_write_raw_and_derived_json -- --ignored --exact --nocapture".to_owned(),
    }
}

fn output_path() -> PathBuf {
    std::env::var_os("MLX_GUARD_CALIBRATION_OUTPUT")
        .map(PathBuf::from)
        .expect("MLX_GUARD_CALIBRATION_OUTPUT must name the JSON output path")
}

#[test]
fn hardware_provenance_omits_unique_machine_identifiers() {
    // Catches publishing serial numbers, UUIDs, UDIDs, or model-order numbers with calibration.
    let raw = "Hardware:\n\n      Model Name: MacBook Pro\n      Model Identifier: MacBookPro18,2\n      Model Number: SECRET-SKU\n      Chip: Apple M1 Max\n      Total Number of Cores: 10\n      Memory: 32 GB\n      Serial Number (system): SECRET-SERIAL\n      Hardware UUID: SECRET-UUID\n      Provisioning UDID: SECRET-UDID\n";

    let sanitized = sanitized_hardware_details(raw);

    assert!(sanitized.contains("Model Name: MacBook Pro"));
    assert!(sanitized.contains("Model Identifier: MacBookPro18,2"));
    assert!(sanitized.contains("Chip: Apple M1 Max"));
    assert!(sanitized.contains("Total Number of Cores: 10"));
    assert!(sanitized.contains("Memory: 32 GB"));
    assert!(!sanitized.contains("SECRET"));
}

#[test]
#[ignore = "bounded M1 Max reference-host calibration"]
fn reference_host_footprint_measurements_write_raw_and_derived_json() {
    // Catches reporting only summaries that cannot be independently recomputed from raw samples.
    let inventory = NativeProcessInventory::new();
    inventory.probe_footprint().unwrap();
    let proc_pid_rusage = measure_native_calls(inventory);
    let anonymous_mapping = measure_mapping(inventory, "allocate", 1);
    let live_shared_mapping = measure_mapping(inventory, "shared-live", 2);
    let metal_mapping = measure_metal(inventory);
    let sixteen_member_sample_window = measure_group_windows(inventory);
    let one_percent = ALLOCATION_BYTES / 100;
    let bounds = vec![
        BoundResult {
            measure: "anonymous 64 MiB maximum magnitude error",
            target: "<= 1%",
            observed: anonymous_mapping.maximum_magnitude_error_bytes,
            unit: "bytes",
            passed: anonymous_mapping.maximum_magnitude_error_bytes <= one_percent,
        },
        BoundResult {
            measure: "live anonymous-shared 64 MiB maximum magnitude error",
            target: "<= 1%",
            observed: live_shared_mapping.maximum_magnitude_error_bytes,
            unit: "bytes",
            passed: live_shared_mapping.maximum_magnitude_error_bytes <= one_percent,
        },
        BoundResult {
            measure: "proc_pid_rusage p95",
            target: "<= 1 ms",
            observed: proc_pid_rusage.p95,
            unit: "nanoseconds",
            passed: proc_pid_rusage.p95 <= 1_000_000,
        },
        BoundResult {
            measure: "Metal 64 MiB maximum visible-delta error",
            target: "<= 10%",
            observed: metal_mapping.maximum_magnitude_error_bytes,
            unit: "bytes",
            passed: metal_mapping.maximum_magnitude_error_bytes <= ALLOCATION_BYTES / 10,
        },
        BoundResult {
            measure: "16-member group sample-window p95",
            target: "<= 10 ms",
            observed: sixteen_member_sample_window.p95,
            unit: "nanoseconds",
            passed: sixteen_member_sample_window.p95 <= 10_000_000,
        },
    ];
    let passed = bounds.iter().all(|bound| bound.passed);
    let result = ReferenceCalibration {
        schema_version: 1,
        provenance: provenance(),
        fixture_ceilings: FixtureCeilings {
            synthetic_allocation_bytes: SYNTHETIC_CEILING_BYTES,
            synthetic_wall_milliseconds: SYNTHETIC_CEILING_MILLISECONDS,
        },
        proc_pid_rusage,
        anonymous_mapping,
        live_shared_mapping,
        metal_mapping,
        sixteen_member_sample_window,
        bounds,
    };
    let path = output_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    assert!(
        passed,
        "one or more predeclared bounds missed; see {path:?}"
    );
}
