#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::time::{Duration, Instant};

use mlx_guard_core::{
    CHECKPOINT_FD_ENV, CheckpointChannel, CheckpointChannelRequestError, CheckpointNonce,
    CheckpointProtocolState, CheckpointRejection, CheckpointSignalConfig, CheckpointWorkerStatus,
    LaunchErrorKind, LaunchOptions, OwnedProcess, SignalResult, StdioMode,
};
#[cfg(target_os = "macos")]
use mlx_guard_core::{FootprintSampler, IdentityTracker, NativeProcessInventory, SampleOutcome};
use mlx_guard_test_support::root_identity;

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");

fn fixture(mode: &str, value: u64, wall_ms: u64) -> LaunchOptions {
    LaunchOptions {
        command: vec![
            OsString::from(FIXTURE),
            OsString::from(mode),
            OsString::from(value.to_string()),
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

fn signal_config(timeout: Duration) -> CheckpointSignalConfig {
    let signal = mlx_guard_core::SignalNumber::new(u8::try_from(libc::SIGUSR1).unwrap()).unwrap();
    CheckpointSignalConfig::new(signal, timeout).unwrap()
}

fn launch(
    mode: &str,
    value: u64,
) -> (
    OwnedProcess,
    BufReader<std::process::ChildStdout>,
    CheckpointChannel,
) {
    let (mut channel, inherited) =
        CheckpointChannel::pair(CheckpointNonce::from_bytes([7; 32])).unwrap();
    let mut process =
        OwnedProcess::launch_with_checkpoint(&fixture(mode, value, 2_000), inherited).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    let mut error = process.take_stderr().unwrap();
    channel.begin_negotiation().unwrap();
    let negotiation_deadline = Instant::now() + Duration::from_secs(3);
    while !channel.poll_ready().unwrap() {
        if let Some(outcome) = process.try_wait_root().unwrap() {
            let mut message = String::new();
            error.read_to_string(&mut message).unwrap();
            panic!("checkpoint worker exited during negotiation: {outcome:?}: {message}");
        }
        if Instant::now() >= negotiation_deadline {
            process.kill_group().unwrap();
            process.wait_root().unwrap();
            let mut message = String::new();
            error.read_to_string(&mut message).unwrap();
            panic!("checkpoint negotiation timed out: {message}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    read_phase(&mut output, "READY mode=checkpoint");
    (process, output, channel)
}

#[test]
fn request_is_refused_until_fd_only_worker_readiness() {
    // Catches signalling a default-terminate user signal before the worker installs its handler.
    let (mut channel, _inherited) =
        CheckpointChannel::pair(CheckpointNonce::from_bytes([7; 32])).unwrap();
    assert_eq!(
        channel
            .begin_request(1, Duration::ZERO, Duration::from_millis(50))
            .unwrap_err(),
        CheckpointChannelRequestError::NotReady
    );
}

fn request_and_signal(
    process: &OwnedProcess,
    channel: &mut CheckpointChannel,
    epoch: Instant,
    timeout: Duration,
) {
    let config = signal_config(timeout);
    channel
        .begin_request(1, epoch.elapsed(), epoch.elapsed() + config.timeout())
        .unwrap();
    let endpoint = process
        .negotiate_checkpoint_endpoint(root_identity(process))
        .unwrap();
    assert_eq!(
        process
            .signal_checkpoint(&endpoint, config.signal())
            .unwrap(),
        SignalResult::Delivered
    );
}

#[test]
fn inherited_channel_authenticates_real_worker_completion() {
    // Catches treating signal delivery or stdout text as checkpoint completion.
    let (mut process, _output, mut channel) = launch("checkpoint-success", 1);
    let epoch = Instant::now();
    request_and_signal(&process, &mut channel, epoch, Duration::from_secs(2));

    let deadline = Instant::now() + Duration::from_secs(2);
    let acknowledgement = loop {
        let poll = channel.poll(epoch.elapsed()).unwrap();
        if let Some(acknowledgement) = poll.acknowledgement {
            break acknowledgement;
        }
        assert!(poll.rejections.is_empty());
        assert!(
            Instant::now() < deadline,
            "worker acknowledgement timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(acknowledgement.status, CheckpointWorkerStatus::Completed);
    assert_eq!(
        channel.state(),
        CheckpointProtocolState::AcknowledgedUnverifiedDurability
    );
    process.kill_group().unwrap();
    process.wait_root().unwrap();
}

#[test]
fn real_worker_spoof_is_rejected_before_matching_completion() {
    // Catches trusting descriptor possession without checking the per-run nonce.
    let (mut process, _output, mut channel) = launch("checkpoint-spoof", 1);
    let epoch = Instant::now();
    request_and_signal(&process, &mut channel, epoch, Duration::from_secs(2));

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut saw_spoof = false;
    let mut acknowledged = false;
    while Instant::now() < deadline && !acknowledged {
        let poll = channel.poll(epoch.elapsed()).unwrap();
        saw_spoof |= poll.rejections.contains(&CheckpointRejection::WrongNonce);
        acknowledged = poll.acknowledgement.is_some();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(saw_spoof);
    assert!(acknowledged);
    process.kill_group().unwrap();
    process.wait_root().unwrap();
}

#[test]
fn reserved_checkpoint_descriptor_environment_cannot_be_overridden() {
    // Catches redirecting the authenticated channel through an attacker-selected descriptor.
    let (_channel, inherited) =
        CheckpointChannel::pair(CheckpointNonce::from_bytes([7; 32])).unwrap();
    let mut options = fixture("checkpoint-success", 1, 100);
    options
        .env
        .insert(CHECKPOINT_FD_ENV.to_owned(), "99".to_owned());
    let error = OwnedProcess::launch_with_checkpoint(&options, inherited).unwrap_err();
    assert_eq!(error.kind(), LaunchErrorKind::InvalidCheckpointChannel);
}

#[test]
fn blocked_handler_times_out_without_blocking_the_supervisor() {
    // Catches a blocking read/join that lets checkpoint work extend the enforcement deadline. The
    // worker blocks for 1.5 s, so a poll that waits on it takes at least that long; a one-second
    // bound separates it from the protocol declaring the request late on its own clock.
    let (mut process, _output, mut channel) = launch("checkpoint-blocked", 1_500);
    let epoch = Instant::now();
    request_and_signal(&process, &mut channel, epoch, Duration::from_millis(30));

    let started = Instant::now();
    let poll = loop {
        let poll = channel.poll(epoch.elapsed()).unwrap();
        if !poll.rejections.is_empty() {
            break poll;
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(poll.rejections, [CheckpointRejection::Late]);
    assert!(started.elapsed() < Duration::from_secs(1));
    process.kill_group().unwrap();
    process.wait_root().unwrap();
}

#[test]
fn endpoint_exit_and_protocol_cancellation_are_typed() {
    // Catches waiting forever on EOF or treating supervisor cancellation as worker success.
    let (mut exited_process, _output, mut exited_channel) = launch("checkpoint-exit", 1);
    let epoch = Instant::now();
    request_and_signal(
        &exited_process,
        &mut exited_channel,
        epoch,
        Duration::from_millis(200),
    );
    exited_process.wait_root().unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let poll = exited_channel.poll(epoch.elapsed()).unwrap();
        if poll
            .rejections
            .contains(&CheckpointRejection::EndpointExited)
        {
            break;
        }
        assert!(Instant::now() < deadline);
    }

    let (mut cancelled_process, mut output, mut cancelled_channel) = launch("checkpoint-cancel", 1);
    cancelled_channel.cancel(Instant::now().elapsed()).unwrap();
    let endpoint = cancelled_process
        .negotiate_checkpoint_endpoint(root_identity(&cancelled_process))
        .unwrap();
    cancelled_process
        .signal_checkpoint(&endpoint, signal_config(Duration::from_millis(50)).signal())
        .unwrap();
    read_phase(&mut output, "CANCELLED");
    cancelled_process.wait_root().unwrap();
    assert_eq!(
        cancelled_channel.state(),
        CheckpointProtocolState::Cancelled
    );
}

/// Size of the mapping the `checkpoint-allocate` worker makes in its handler before it acknowledges.
#[cfg(target_os = "macos")]
const ALLOCATION_BYTES: u64 = 8 * 1024 * 1024;

/// What the sample-then-poll loop saw, so a caller that forces an interleaving can prove it did.
#[cfg(target_os = "macos")]
struct AllocationLoop {
    passes: u32,
    growth_before_ack: bool,
}

/// Request a checkpoint from an allocating worker, sample its footprint and poll for the
/// acknowledgement until it arrives, then assert the handler's allocation was observed.
/// `between_sample_and_poll` runs once per pass, after that pass's sample and before its poll.
#[cfg(target_os = "macos")]
fn observe_allocation_until_acknowledged(
    process: &mut OwnedProcess,
    channel: &mut CheckpointChannel,
    mut between_sample_and_poll: impl FnMut(),
) -> AllocationLoop {
    let inventory = NativeProcessInventory::new();
    let tracker = IdentityTracker::new(root_identity(process), process.process_group_id()).unwrap();
    let config = mlx_guard_core::SamplingConfig::new(
        Duration::from_millis(10),
        Duration::from_millis(100),
        Duration::from_millis(10),
        8,
    )
    .unwrap();
    let mut sampler = FootprintSampler::new(config, tracker);
    let epoch = Instant::now();
    let baseline = match sampler.sample_native(&inventory, epoch).outcome {
        SampleOutcome::Complete { total_bytes } => total_bytes,
        ref outcome => panic!("unexpected baseline: {outcome:?}"),
    };
    let grew = |outcome: &SampleOutcome| {
        matches!(outcome, SampleOutcome::Complete { total_bytes }
            if *total_bytes >= baseline + ALLOCATION_BYTES / 2)
    };
    request_and_signal(process, channel, epoch, Duration::from_secs(2));

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut passes = 0;
    let mut observed_growth = false;
    let mut acknowledged = false;
    while Instant::now() < deadline && !acknowledged {
        passes += 1;
        observed_growth |= grew(&sampler.sample_native(&inventory, epoch).outcome);
        between_sample_and_poll();
        acknowledged = channel
            .poll(epoch.elapsed())
            .unwrap()
            .acknowledgement
            .is_some();
    }
    let growth_before_ack = observed_growth;
    // The acknowledgement can land between a pass's sample and its poll, which ends the loop before
    // any sample saw the mapping. The handler keeps the mapping after acknowledging, so one more
    // sample here still sees it.
    observed_growth |= grew(&sampler.sample_native(&inventory, epoch).outcome);
    assert!(observed_growth);
    assert!(acknowledged);
    AllocationLoop {
        passes,
        growth_before_ack,
    }
}

#[test]
#[cfg(target_os = "macos")]
fn checkpoint_handler_allocation_is_seen_by_the_native_sampler() {
    // Catches a native sampler that misses the 8 MiB mapping a checkpoint handler makes before it
    // acknowledges: no sample, before or after the acknowledgement, then reads 4 MiB of growth.
    let (mut process, _output, mut channel) = launch("checkpoint-allocate", ALLOCATION_BYTES);
    observe_allocation_until_acknowledged(&mut process, &mut channel, || {});
    process.kill_group().unwrap();
    process.wait_root().unwrap();
}

#[cfg(target_os = "macos")]
fn send_signal(signal: &str, pid: i32) {
    let status = std::process::Command::new("/bin/kill")
        .args([signal, &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill {signal} {pid}: {status}");
}

#[cfg(target_os = "macos")]
fn is_stopped(pid: i32) -> bool {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim_start()
        .starts_with('T')
}

#[test]
#[cfg(target_os = "macos")]
fn allocation_stays_observed_when_the_ack_lands_between_a_sample_and_its_poll() {
    // Catches ending the observation at the acknowledgement: with the worker held stopped until the
    // first pass has sampled, the handler allocates and acknowledges before that pass polls, so no
    // in-loop sample can see the mapping.
    let (mut process, mut output, mut channel) = launch("checkpoint-allocate", ALLOCATION_BYTES);
    let pid = process.root_pid().cast_signed();
    send_signal("-STOP", pid);
    let stopped_by = Instant::now() + Duration::from_secs(1);
    while !is_stopped(pid) {
        assert!(Instant::now() < stopped_by, "the worker never stopped");
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut released = false;
    let forced = observe_allocation_until_acknowledged(&mut process, &mut channel, || {
        if released {
            return;
        }
        released = true;
        // Resuming the worker lets it handle the checkpoint signal that waited while it was stopped.
        send_signal("-CONT", pid);
        // The worker writes ACKED only after its acknowledgement is flushed to the channel, so the
        // poll that follows finds it; the worker's own watchdog bounds this read.
        read_phase(&mut output, "ACKED");
    });
    assert_eq!(
        forced.passes, 1,
        "the acknowledgement missed the first poll"
    );
    assert!(!forced.growth_before_ack, "a pass sampled the mapping");
    process.kill_group().unwrap();
    process.wait_root().unwrap();
}
