#![cfg(unix)]

//! Delivery-time identity revalidation for the negotiated cooperative checkpoint endpoint.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::time::{Duration, Instant};

use mlx_guard_core::{
    CheckpointChannel, CheckpointNonce, CheckpointWorkerStatus, ControlErrorKind, LaunchOptions,
    OwnedProcess, ProcessIdentity, SignalResult, StdioMode, checkpoint_signal_usr1,
};
use mlx_guard_test_support::root_identity;

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");

/// Fixture wall-clock ceiling for this test's worker.
///
/// The test deliberately spends [`REFUSAL_SILENCE`] observing a live worker doing nothing, so the
/// usual two-second fixture ceiling would race its own watchdog. Nine seconds stays inside the
/// crate's hard ten-second limit, and the test kills the group as soon as it is done, so this is a
/// ceiling and not a duration.
const FIXTURE_WALL_MS: u64 = 9_000;

/// How long a live worker must stay silent for the refusal to count as behavioural.
///
/// The reference host's published acknowledgement p95 is 29 ms; a second is over thirty times that,
/// which keeps the window meaningful on the three-vCPU CI runner without making the suite slow.
const REFUSAL_SILENCE: Duration = Duration::from_secs(1);

/// Wait-until deadline for the positive control's acknowledgement.
const ACKNOWLEDGEMENT_DEADLINE: Duration = Duration::from_secs(3);

/// Wait-until deadline for FD-only readiness negotiation.
const NEGOTIATION_DEADLINE: Duration = Duration::from_secs(3);

fn fixture(mode: &str) -> LaunchOptions {
    LaunchOptions {
        command: vec![
            OsString::from(FIXTURE),
            OsString::from(mode),
            OsString::from("1"),
            OsString::from(FIXTURE_WALL_MS.to_string()),
        ],
        cwd: None,
        clear_env: false,
        env: BTreeMap::new(),
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    }
}

/// Launch a cooperative worker and return it once it has announced checkpoint readiness.
fn launch(mode: &str) -> (OwnedProcess, CheckpointChannel) {
    let (mut channel, inherited) =
        CheckpointChannel::pair(CheckpointNonce::from_bytes([5; 32])).unwrap();
    let mut process = OwnedProcess::launch_with_checkpoint(&fixture(mode), inherited).unwrap();
    let mut output = BufReader::new(process.take_stdout().unwrap());
    let mut error = process.take_stderr().unwrap();
    channel.begin_negotiation().unwrap();
    let deadline = Instant::now() + NEGOTIATION_DEADLINE;
    while !channel.poll_ready().unwrap() {
        if let Some(outcome) = process.try_wait_root().unwrap() {
            let mut message = String::new();
            error.read_to_string(&mut message).unwrap();
            panic!("checkpoint worker exited during negotiation: {outcome:?}: {message}");
        }
        assert!(
            Instant::now() < deadline,
            "checkpoint negotiation timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert!(
        line.starts_with("READY mode=checkpoint"),
        "unexpected worker phase {line:?}"
    );
    (process, channel)
}

#[test]
fn a_checkpoint_endpoint_whose_start_token_no_longer_matches_is_refused_before_delivery() {
    // Catches delivering the checkpoint request on PID and process group alone: drop the start-token
    // comparison from the delivery path and the wrong-token request is signalled, the live worker
    // acknowledges it, and both halves of this test fail.
    let (mut process, mut channel) = launch("checkpoint-success");
    let epoch = Instant::now();
    let root = root_identity(&process);

    // One request with one generous deadline covers both halves, so the acknowledgement asserted at
    // the end can only have come from the second, identity-matching delivery.
    channel
        .begin_request(1, epoch.elapsed(), epoch.elapsed() + Duration::from_secs(8))
        .unwrap();

    // Negotiation takes the caller's identity on trust, so a wrong start token can be bound to a
    // real, live, in-group PID exactly as a recycled PID would present itself at delivery.
    let stale = ProcessIdentity {
        pid: root.pid,
        start_abstime: root.start_abstime.wrapping_add(1),
    };
    let stale_endpoint = process.negotiate_checkpoint_endpoint(stale).unwrap();
    let refusal = process
        .signal_checkpoint(&stale_endpoint, checkpoint_signal_usr1())
        .unwrap_err();
    assert_eq!(
        refusal.kind(),
        ControlErrorKind::InvalidCheckpointEndpoint,
        "a stale endpoint must be refused as an endpoint, not as a failed signal"
    );

    // The refusal has to be behavioural and not merely a return value: nothing may reach the worker.
    let silence_deadline = Instant::now() + REFUSAL_SILENCE;
    while Instant::now() < silence_deadline {
        let poll = channel.poll(epoch.elapsed()).unwrap();
        assert!(
            poll.acknowledgement.is_none(),
            "the refused request reached the worker"
        );
        assert!(
            poll.rejections.is_empty(),
            "unexpected protocol rejection during the refusal window: {:?}",
            poll.rejections
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    // Positive control, same worker and same in-flight request: the silence above was this fix
    // refusing delivery, not a starved runner that had not got round to acknowledging yet.
    let endpoint = process.negotiate_checkpoint_endpoint(root).unwrap();
    assert_eq!(
        process
            .signal_checkpoint(&endpoint, checkpoint_signal_usr1())
            .unwrap(),
        SignalResult::Delivered
    );
    let deadline = Instant::now() + ACKNOWLEDGEMENT_DEADLINE;
    let acknowledgement = loop {
        let poll = channel.poll(epoch.elapsed()).unwrap();
        if let Some(acknowledgement) = poll.acknowledgement {
            break acknowledgement;
        }
        assert!(
            poll.rejections.is_empty(),
            "unexpected protocol rejection after a valid delivery: {:?}",
            poll.rejections
        );
        assert!(
            Instant::now() < deadline,
            "the identity-matching delivery was never acknowledged"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(acknowledgement.status, CheckpointWorkerStatus::Completed);

    process.kill_group().unwrap();
    process.wait_root().unwrap();
}
