#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use mlx_guard_core::{
    FootprintSampler, IdentityTracker, LaunchOptions, NativeAdvisoryObserver,
    NativeProcessInventory, ObserveCalibration, Observed, OwnedProcess, RootOutcome, SampleOutcome,
    SamplingConfig, StdioMode,
};

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");
const ALLOCATION_BYTES: u64 = 32 * 1024 * 1024;

fn fixture() -> LaunchOptions {
    LaunchOptions {
        command: vec![
            OsString::from(FIXTURE),
            OsString::from("allocate"),
            OsString::from(ALLOCATION_BYTES.to_string()),
            OsString::from("3000"),
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

fn complete_bytes(sample: &mlx_guard_core::FootprintSample) -> u64 {
    match sample.outcome {
        SampleOutcome::Complete { total_bytes } => total_bytes,
        ref outcome => panic!("expected complete sample, got {outcome:?}"),
    }
}

#[test]
fn observe_calibration_follows_real_sampler_growth_without_intervening() {
    // Catches synthetic calibration, a separate sampler path, or observe-mode process signalling.
    let inventory = NativeProcessInventory::new();
    inventory.probe_footprint().unwrap();
    let mut process = OwnedProcess::launch(&fixture()).unwrap();
    let mut input = process.take_stdin().unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    read_phase(&mut output, "READY mode=allocate");

    let root = inventory
        .inspect(process.root_pid().cast_signed())
        .unwrap()
        .identity;
    let tracker = IdentityTracker::new(root, process.process_group_id()).unwrap();
    let config = SamplingConfig::new(
        Duration::from_millis(10),
        Duration::from_millis(100),
        Duration::from_millis(10),
        8,
    )
    .unwrap();
    let mut sampler = FootprintSampler::new(config, tracker);
    let observer = NativeAdvisoryObserver::new();
    let mut calibration = ObserveCalibration::new();
    let epoch = Instant::now();

    let baseline = sampler.sample_native(&inventory, epoch).clone();
    let baseline_bytes = complete_bytes(&baseline);
    let advisory = observer.snapshot(baseline.finished_at, Duration::from_secs(5));
    let _ = calibration.record_sample(&baseline, epoch.elapsed(), &advisory);

    send(&mut input, "allocate");
    read_phase(&mut output, "ALLOCATED");
    let allocated = sampler.sample_native(&inventory, epoch).clone();
    let allocated_bytes = complete_bytes(&allocated);
    assert!(allocated_bytes >= baseline_bytes + ALLOCATION_BYTES * 99 / 100);
    let advisory = observer.snapshot(allocated.finished_at, Duration::from_secs(5));
    let window = calibration.record_sample(&allocated, epoch.elapsed(), &advisory);
    assert!(matches!(
        window.advisory.growth_bytes_per_second,
        Observed::Available { value } if value > 0
    ));

    send(&mut input, "release");
    read_phase(&mut output, "RELEASED");
    let released = sampler.sample_native(&inventory, epoch).clone();
    let advisory = observer.snapshot(released.finished_at, Duration::from_secs(5));
    let _ = calibration.record_sample(&released, epoch.elapsed(), &advisory);

    let artifact = calibration.artifact();
    assert_eq!(artifact.total_samples, 3);
    assert_eq!(artifact.complete_samples, 3);
    assert_eq!(artifact.incomplete_samples, 0);
    assert_eq!(artifact.automatic_limit_bytes, None);
    assert!(!artifact.safety_certified);
    assert_eq!(calibration.intervention_count(), 0);
    assert!(matches!(
        artifact.peak_aggregate_footprint_bytes,
        Observed::Available { value } if value >= allocated_bytes
    ));

    send(&mut input, "exit");
    read_phase(&mut output, "EXIT");
    assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(0));
}
