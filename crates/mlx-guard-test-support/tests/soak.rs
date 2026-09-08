#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::thread;
use std::time::{Duration, Instant};

use mlx_guard_core::{
    FootprintSampler, IdentityTracker, LaunchOptions, NativeProcessInventory, OwnedProcess,
    SamplingConfig, StdioMode,
};

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
