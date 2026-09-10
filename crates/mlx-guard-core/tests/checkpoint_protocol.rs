#![cfg(unix)]

use std::time::Duration;

use mlx_guard_core::{
    CheckpointAcknowledgement, CheckpointArtifactKind, CheckpointArtifactMetadata, CheckpointHello,
    CheckpointNonce, CheckpointProtocol, CheckpointProtocolState, CheckpointRejection,
    CheckpointRequest, CheckpointSignalConfig, CheckpointSignalConfigError, CheckpointWorkerStatus,
    MAX_CHECKPOINT_FRAME_BYTES, SignalNumber,
};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn nonce(byte: u8) -> CheckpointNonce {
    CheckpointNonce::from_bytes([byte; 32])
}

fn started_protocol() -> (CheckpointProtocol, Vec<u8>) {
    let mut protocol = CheckpointProtocol::new(nonce(7));
    let request = protocol.begin_request(42, ms(10), ms(60)).unwrap();
    (protocol, request)
}

#[test]
fn matching_completion_round_trips_only_redacted_artifact_metadata() {
    // Catches accepting signal delivery as success or placing an artifact path on the wire.
    assert_eq!(format!("{:?}", nonce(7)), "CheckpointNonce(<redacted>)");
    let (mut protocol, request_frame) = started_protocol();
    let request = mlx_guard_core::CheckpointRequest::decode(&request_frame).unwrap();
    assert_eq!(request.version(), 1);
    assert_eq!(request.request_id(), 42);
    assert_eq!(request.deadline_at(), ms(60));
    assert_eq!(
        protocol.state(),
        CheckpointProtocolState::RequestedUnverified
    );

    let acknowledgement = CheckpointAcknowledgement::new(
        nonce(7),
        42,
        CheckpointWorkerStatus::Completed,
        Some(CheckpointArtifactMetadata {
            kind: CheckpointArtifactKind::File,
            size_bytes: Some(12_345),
        }),
    )
    .encode();
    let poll = protocol.ingest(ms(20), &acknowledgement);
    assert!(poll.rejections.is_empty());
    let accepted = poll.acknowledgement.unwrap();
    assert_eq!(accepted.status, CheckpointWorkerStatus::Completed);
    assert_eq!(
        accepted.artifact,
        Some(CheckpointArtifactMetadata {
            kind: CheckpointArtifactKind::File,
            size_bytes: Some(12_345),
        })
    );
    assert_eq!(
        protocol.state(),
        CheckpointProtocolState::AcknowledgedUnverifiedDurability
    );
}

#[test]
fn worker_failure_and_cancellation_never_become_success() {
    // Catches mapping every authenticated status to completed checkpoint evidence.
    for (status, expected_state) in [
        (
            CheckpointWorkerStatus::Failed,
            CheckpointProtocolState::WorkerFailed,
        ),
        (
            CheckpointWorkerStatus::Cancelled,
            CheckpointProtocolState::Cancelled,
        ),
    ] {
        let (mut protocol, _) = started_protocol();
        let acknowledgement = CheckpointAcknowledgement::new(nonce(7), 42, status, None).encode();
        let poll = protocol.ingest(ms(20), &acknowledgement);
        assert_eq!(poll.acknowledgement.unwrap().status, status);
        assert_eq!(protocol.state(), expected_state);
        assert_ne!(
            protocol.state(),
            CheckpointProtocolState::AcknowledgedUnverifiedDurability
        );
    }
}

#[test]
fn spoof_replay_duplicate_and_malformed_frames_never_authenticate() {
    // Catches nonce-blind, request-id-blind, duplicate-accepting, or unbounded parsers.
    let mut wrong_version =
        CheckpointAcknowledgement::new(nonce(7), 42, CheckpointWorkerStatus::Completed, None)
            .encode();
    wrong_version[8] = 2;
    let mut hidden_artifact_size = CheckpointAcknowledgement::new(
        nonce(7),
        42,
        CheckpointWorkerStatus::Completed,
        Some(CheckpointArtifactMetadata {
            kind: CheckpointArtifactKind::File,
            size_bytes: None,
        }),
    )
    .encode();
    *hidden_artifact_size.last_mut().unwrap() = 1;
    let attacks = vec![
        (
            CheckpointAcknowledgement::new(nonce(8), 42, CheckpointWorkerStatus::Completed, None)
                .encode(),
            CheckpointRejection::WrongNonce,
        ),
        (
            CheckpointAcknowledgement::new(nonce(7), 41, CheckpointWorkerStatus::Completed, None)
                .encode(),
            CheckpointRejection::Replay,
        ),
        (
            vec![0, 0, 0, 3, 0xff, 0xff, 0xff],
            CheckpointRejection::Malformed,
        ),
        (wrong_version, CheckpointRejection::Malformed),
        (hidden_artifact_size, CheckpointRejection::Malformed),
    ];
    for (frame, expected) in attacks {
        let (mut protocol, _) = started_protocol();
        let poll = protocol.ingest(ms(20), &frame);
        assert_eq!(poll.rejections, [expected]);
        assert!(poll.acknowledgement.is_none());
        assert_eq!(
            protocol.state(),
            CheckpointProtocolState::RequestedUnverified
        );
    }

    let (mut protocol, _) = started_protocol();
    let acknowledgement =
        CheckpointAcknowledgement::new(nonce(7), 42, CheckpointWorkerStatus::Completed, None)
            .encode();
    assert!(
        protocol
            .ingest(ms(20), &acknowledgement)
            .acknowledgement
            .is_some()
    );
    let duplicate = protocol.ingest(ms(21), &acknowledgement);
    assert_eq!(duplicate.rejections, [CheckpointRejection::Duplicate]);
    assert!(duplicate.acknowledgement.is_none());

    let (mut protocol, _) = started_protocol();
    let oversized_length = u32::try_from(MAX_CHECKPOINT_FRAME_BYTES + 1)
        .unwrap()
        .to_be_bytes();
    let oversized = protocol.ingest(ms(20), &oversized_length);
    assert_eq!(oversized.rejections, [CheckpointRejection::Oversized]);
    assert_eq!(protocol.buffered_bytes(), 0);
}

#[test]
fn partial_late_and_post_exit_messages_cannot_extend_the_deadline() {
    // Catches blocking for a partial frame or accepting data after timeout/endpoint exit.
    let acknowledgement =
        CheckpointAcknowledgement::new(nonce(7), 42, CheckpointWorkerStatus::Completed, None)
            .encode();

    let (mut partial_protocol, _) = started_protocol();
    let partial = partial_protocol.ingest(ms(20), &acknowledgement[..8]);
    assert!(partial.rejections.is_empty());
    assert!(partial.acknowledgement.is_none());
    let exited = partial_protocol.endpoint_exited(ms(21));
    assert_eq!(
        exited.rejections,
        [
            CheckpointRejection::Partial,
            CheckpointRejection::EndpointExited
        ]
    );
    assert_eq!(partial_protocol.state(), CheckpointProtocolState::Cancelled);

    let (mut late_protocol, _) = started_protocol();
    let late = late_protocol.ingest(ms(60), &acknowledgement);
    assert_eq!(late.rejections, [CheckpointRejection::Late]);
    assert_eq!(late_protocol.state(), CheckpointProtocolState::TimedOut);

    let (mut exited_protocol, _) = started_protocol();
    let _ = exited_protocol.endpoint_exited(ms(20));
    let post_exit = exited_protocol.ingest(ms(21), &acknowledgement);
    assert_eq!(post_exit.rejections, [CheckpointRejection::PostExit]);
    assert!(post_exit.acknowledgement.is_none());

    let (mut cancelled_protocol, _) = started_protocol();
    let _ = cancelled_protocol.cancel(ms(20));
    let post_cancel = cancelled_protocol.ingest(ms(21), &acknowledgement);
    assert_eq!(post_cancel.rejections, [CheckpointRejection::PostCancel]);
    assert!(post_cancel.acknowledgement.is_none());
}

#[test]
fn checkpoint_signal_configuration_rejects_default_action_collisions() {
    // Catches enabling a checkpoint signal that collides with TERM/KILL or has no deadline.
    let user_signal = SignalNumber::new(u8::try_from(libc::SIGUSR1).unwrap()).unwrap();
    assert!(CheckpointSignalConfig::new(user_signal, ms(50)).is_ok());
    let term = SignalNumber::new(u8::try_from(libc::SIGTERM).unwrap()).unwrap();
    assert_eq!(
        CheckpointSignalConfig::new(term, ms(50)).unwrap_err(),
        CheckpointSignalConfigError::SignalCollision
    );
    assert_eq!(
        CheckpointSignalConfig::new(user_signal, Duration::ZERO).unwrap_err(),
        CheckpointSignalConfigError::InvalidTimeout
    );
}

#[test]
fn wire_format_v1_golden_frames() {
    // Pins the byte layout the protocol document publishes, so a reordered field, a changed kind
    // byte, or a widened length header goes red here before a worker built against the document
    // stops authenticating.
    let nonce_bytes = [0x11_u8; 32];
    let nonce = CheckpointNonce::from_bytes(nonce_bytes);
    let magic_version = |kind: u8| {
        let mut prefix = b"MGCP".to_vec();
        prefix.push(1);
        prefix.push(kind);
        prefix
    };

    let mut hello = vec![0, 0, 0, 38];
    hello.extend(magic_version(3));
    hello.extend_from_slice(&nonce_bytes);
    let mut ready = vec![0, 0, 0, 38];
    ready.extend(magic_version(4));
    ready.extend_from_slice(&nonce_bytes);
    assert_eq!(
        CheckpointHello::decode(&hello).unwrap().ready_frame(),
        ready
    );

    let mut protocol = CheckpointProtocol::new(nonce);
    let request = protocol
        .begin_request(
            0x0102_0304_0506_0708,
            ms(500),
            Duration::from_nanos(1_500_000_000),
        )
        .unwrap();
    let mut expected_request = vec![0, 0, 0, 54];
    expected_request.extend(magic_version(1));
    expected_request.extend_from_slice(&nonce_bytes);
    expected_request.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    expected_request.extend_from_slice(&[0, 0, 0, 0, 0x59, 0x68, 0x2F, 0x00]);
    assert_eq!(request, expected_request);
    let decoded = CheckpointRequest::decode(&request).unwrap();
    assert_eq!(decoded.request_id(), 0x0102_0304_0506_0708);
    assert_eq!(decoded.deadline_at(), Duration::from_nanos(1_500_000_000));

    // Every status and artifact-kind value, with has-size both ways, and no two bytes at the same
    // position equal across cases, so a swapped push or a remapped value shows up.
    let cases = [
        (
            CheckpointWorkerStatus::Completed,
            Some(CheckpointArtifactMetadata {
                kind: CheckpointArtifactKind::Directory,
                size_bytes: Some(4096),
            }),
            [1, 2, 1],
            [0, 0, 0, 0, 0, 0, 0x10, 0x00],
        ),
        (
            CheckpointWorkerStatus::Failed,
            Some(CheckpointArtifactMetadata {
                kind: CheckpointArtifactKind::Opaque,
                size_bytes: None,
            }),
            [2, 3, 0],
            [0; 8],
        ),
        (
            CheckpointWorkerStatus::Cancelled,
            Some(CheckpointArtifactMetadata {
                kind: CheckpointArtifactKind::File,
                size_bytes: Some(0),
            }),
            [3, 1, 1],
            [0; 8],
        ),
        (CheckpointWorkerStatus::Completed, None, [1, 0, 0], [0; 8]),
    ];
    for (status, artifact, tail, size) in cases {
        let acknowledgement =
            CheckpointAcknowledgement::new(nonce, 0x0102_0304_0506_0708, status, artifact).encode();
        let mut expected = vec![0, 0, 0, 57];
        expected.extend(magic_version(2));
        expected.extend_from_slice(&nonce_bytes);
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        expected.extend_from_slice(&tail);
        expected.extend_from_slice(&size);
        assert_eq!(acknowledgement, expected, "{status:?} {artifact:?}");
    }
}
