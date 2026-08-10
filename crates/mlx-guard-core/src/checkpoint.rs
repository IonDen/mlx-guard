use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::SignalNumber;

const FRAME_HEADER_BYTES: usize = 4;
const MAGIC: [u8; 4] = *b"MGCP";
const REQUEST_KIND: u8 = 1;
const ACKNOWLEDGEMENT_KIND: u8 = 2;
const HELLO_KIND: u8 = 3;
const READY_KIND: u8 = 4;
const REQUEST_BODY_BYTES: usize = 54;
const ACKNOWLEDGEMENT_BODY_BYTES: usize = 57;
const NEGOTIATION_BODY_BYTES: usize = 38;

/// Version of the inherited-FD checkpoint protocol.
pub const CHECKPOINT_PROTOCOL_VERSION: u8 = 1;

/// Maximum body length accepted from an inherited checkpoint descriptor.
pub const MAX_CHECKPOINT_FRAME_BYTES: usize = 128;

/// Reserved child environment key containing the inherited checkpoint descriptor number.
pub const CHECKPOINT_FD_ENV: &str = "MLX_GUARD_CHECKPOINT_FD";

/// A per-run capability value. Debug output deliberately hides its bytes.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct CheckpointNonce([u8; 32]);

impl CheckpointNonce {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Read a fresh nonce from the operating system random device.
    ///
    /// # Errors
    ///
    /// Returns the redacted I/O failure when secure random bytes cannot be read.
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0_u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(Self(bytes))
    }
}

impl fmt::Debug for CheckpointNonce {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CheckpointNonce(<redacted>)")
    }
}

/// Supervisor hello decoded by the inherited-FD worker helper before signal installation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointHello {
    nonce: CheckpointNonce,
}

impl CheckpointHello {
    /// Decode one versioned negotiation hello.
    ///
    /// # Errors
    ///
    /// Returns a malformed-frame error for an unexpected length, magic, version, or kind.
    pub fn decode(frame: &[u8]) -> Result<Self, CheckpointProtocolError> {
        decode_negotiation(frame, HELLO_KIND).map(|nonce| Self { nonce })
    }

    /// Encode the matching readiness response after the signal handler is installed.
    #[must_use]
    pub fn ready_frame(self) -> Vec<u8> {
        encode_negotiation(READY_KIND, self.nonce)
    }
}

/// Worker-reported checkpoint result. Only `Completed` becomes a policy acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointWorkerStatus {
    Completed,
    Failed,
    Cancelled,
}

/// Path-free artifact classification carried by an acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointArtifactKind {
    File,
    Directory,
    Opaque,
}

/// Optional, redacted artifact facts. Paths and names have no wire representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointArtifactMetadata {
    pub kind: CheckpointArtifactKind,
    pub size_bytes: Option<u64>,
}

/// One decoded request delivered over the inherited descriptor before endpoint signalling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointRequest {
    nonce: CheckpointNonce,
    request_id: u64,
    deadline_at: Duration,
}

impl CheckpointRequest {
    /// Decode exactly one bounded request frame.
    ///
    /// # Errors
    ///
    /// Returns [`CheckpointProtocolError::MalformedFrame`] for an invalid length, version, kind,
    /// nonce-independent field, or monotonic deadline.
    pub fn decode(frame: &[u8]) -> Result<Self, CheckpointProtocolError> {
        let body = exact_body(frame, REQUEST_BODY_BYTES)?;
        if body[..4] != MAGIC || body[4] != CHECKPOINT_PROTOCOL_VERSION || body[5] != REQUEST_KIND {
            return Err(CheckpointProtocolError::MalformedFrame);
        }
        let nonce = CheckpointNonce(copy_array::<32>(&body[6..38])?);
        let request_id = u64::from_be_bytes(copy_array::<8>(&body[38..46])?);
        let deadline_ns = u64::from_be_bytes(copy_array::<8>(&body[46..54])?);
        if request_id == 0 || deadline_ns == 0 {
            return Err(CheckpointProtocolError::MalformedFrame);
        }
        Ok(Self {
            nonce,
            request_id,
            deadline_at: Duration::from_nanos(deadline_ns),
        })
    }

    #[must_use]
    pub const fn version(&self) -> u8 {
        CHECKPOINT_PROTOCOL_VERSION
    }

    #[must_use]
    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    #[must_use]
    pub const fn deadline_at(&self) -> Duration {
        self.deadline_at
    }
}

/// A worker acknowledgement before nonce, request, deadline, and duplicate validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointAcknowledgement {
    nonce: CheckpointNonce,
    request_id: u64,
    pub status: CheckpointWorkerStatus,
    pub artifact: Option<CheckpointArtifactMetadata>,
}

impl CheckpointAcknowledgement {
    #[must_use]
    pub const fn new(
        nonce: CheckpointNonce,
        request_id: u64,
        status: CheckpointWorkerStatus,
        artifact: Option<CheckpointArtifactMetadata>,
    ) -> Self {
        Self {
            nonce,
            request_id,
            status,
            artifact,
        }
    }

    #[must_use]
    pub fn for_request(
        request: &CheckpointRequest,
        status: CheckpointWorkerStatus,
        artifact: Option<CheckpointArtifactMetadata>,
    ) -> Self {
        Self::new(request.nonce, request.request_id, status, artifact)
    }

    #[must_use]
    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let mut body = Vec::with_capacity(ACKNOWLEDGEMENT_BODY_BYTES);
        body.extend_from_slice(&MAGIC);
        body.push(CHECKPOINT_PROTOCOL_VERSION);
        body.push(ACKNOWLEDGEMENT_KIND);
        body.extend_from_slice(&self.nonce.0);
        body.extend_from_slice(&self.request_id.to_be_bytes());
        body.push(match self.status {
            CheckpointWorkerStatus::Completed => 1,
            CheckpointWorkerStatus::Failed => 2,
            CheckpointWorkerStatus::Cancelled => 3,
        });
        let (kind, has_size, size) = match self.artifact {
            None => (0, 0, 0),
            Some(metadata) => (
                match metadata.kind {
                    CheckpointArtifactKind::File => 1,
                    CheckpointArtifactKind::Directory => 2,
                    CheckpointArtifactKind::Opaque => 3,
                },
                u8::from(metadata.size_bytes.is_some()),
                metadata.size_bytes.unwrap_or(0),
            ),
        };
        body.push(kind);
        body.push(has_size);
        body.extend_from_slice(&size.to_be_bytes());
        encode_body(&body)
    }
}

/// Stable state of one run's at-most-once checkpoint exchange.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointProtocolState {
    Idle,
    RequestedUnverified,
    AcknowledgedUnverifiedDurability,
    WorkerFailed,
    TimedOut,
    Cancelled,
}

/// Invalid or unauthenticated checkpoint input retained for diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointRejection {
    WrongNonce,
    Replay,
    Duplicate,
    Malformed,
    Oversized,
    Partial,
    Late,
    EndpointExited,
    PostExit,
}

/// Bounded result of one nonblocking receive attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointPoll {
    pub acknowledgement: Option<CheckpointAcknowledgement>,
    pub rejections: Vec<CheckpointRejection>,
}

impl CheckpointPoll {
    fn pending() -> Self {
        Self {
            acknowledgement: None,
            rejections: Vec::new(),
        }
    }

    fn rejected(reason: CheckpointRejection) -> Self {
        Self {
            acknowledgement: None,
            rejections: vec![reason],
        }
    }
}

/// Invalid checkpoint protocol construction or request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointProtocolError {
    ActiveRequest,
    InvalidRequest,
    DeadlineOutOfRange,
    MalformedFrame,
}

impl fmt::Display for CheckpointProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ActiveRequest => "checkpoint request is already active",
            Self::InvalidRequest => "checkpoint request or deadline is invalid",
            Self::DeadlineOutOfRange => "checkpoint deadline exceeds the wire representation",
            Self::MalformedFrame => "checkpoint frame is malformed",
        })
    }
}

impl Error for CheckpointProtocolError {}

/// Pure, fixed-memory protocol state used by a nonblocking inherited-FD adapter.
#[derive(Debug)]
pub struct CheckpointProtocol {
    nonce: CheckpointNonce,
    state: CheckpointProtocolState,
    active_request_id: Option<u64>,
    deadline_at: Option<Duration>,
    buffer: Vec<u8>,
    endpoint_exited: bool,
}

impl CheckpointProtocol {
    #[must_use]
    pub fn new(nonce: CheckpointNonce) -> Self {
        Self {
            nonce,
            state: CheckpointProtocolState::Idle,
            active_request_id: None,
            deadline_at: None,
            buffer: Vec::with_capacity(MAX_CHECKPOINT_FRAME_BYTES + FRAME_HEADER_BYTES),
            endpoint_exited: false,
        }
    }

    /// Start the run's single checkpoint request and return its bounded wire frame.
    ///
    /// `requested_at` and `deadline_at` share the supervisor's monotonic run epoch.
    ///
    /// # Errors
    ///
    /// Returns a typed error for duplicate requests, zero IDs, reversed deadlines, or deadlines that
    /// do not fit the v1 nanosecond field.
    pub fn begin_request(
        &mut self,
        request_id: u64,
        requested_at: Duration,
        deadline_at: Duration,
    ) -> Result<Vec<u8>, CheckpointProtocolError> {
        if self.state != CheckpointProtocolState::Idle {
            return Err(CheckpointProtocolError::ActiveRequest);
        }
        if request_id == 0 || deadline_at <= requested_at || self.endpoint_exited {
            return Err(CheckpointProtocolError::InvalidRequest);
        }
        let deadline_ns = u64::try_from(deadline_at.as_nanos())
            .map_err(|_| CheckpointProtocolError::DeadlineOutOfRange)?;
        let mut body = Vec::with_capacity(REQUEST_BODY_BYTES);
        body.extend_from_slice(&MAGIC);
        body.push(CHECKPOINT_PROTOCOL_VERSION);
        body.push(REQUEST_KIND);
        body.extend_from_slice(&self.nonce.0);
        body.extend_from_slice(&request_id.to_be_bytes());
        body.extend_from_slice(&deadline_ns.to_be_bytes());
        self.state = CheckpointProtocolState::RequestedUnverified;
        self.active_request_id = Some(request_id);
        self.deadline_at = Some(deadline_at);
        Ok(encode_body(&body))
    }

    /// Ingest at most one bounded chunk already read without blocking from the inherited channel.
    #[must_use]
    pub fn ingest(&mut self, at: Duration, bytes: &[u8]) -> CheckpointPoll {
        if self.endpoint_exited {
            return CheckpointPoll::rejected(CheckpointRejection::PostExit);
        }
        if self.state == CheckpointProtocolState::AcknowledgedUnverifiedDurability
            || self.state == CheckpointProtocolState::WorkerFailed
        {
            return CheckpointPoll::rejected(CheckpointRejection::Duplicate);
        }
        if self.state == CheckpointProtocolState::TimedOut {
            return CheckpointPoll::rejected(CheckpointRejection::Late);
        }
        if self
            .deadline_at
            .is_some_and(|deadline_at| at >= deadline_at)
        {
            self.state = CheckpointProtocolState::TimedOut;
            self.buffer.clear();
            return CheckpointPoll::rejected(CheckpointRejection::Late);
        }
        if self.state != CheckpointProtocolState::RequestedUnverified {
            return CheckpointPoll::rejected(CheckpointRejection::Malformed);
        }
        let frame = match self.receive_frame(bytes) {
            Ok(Some(frame)) => frame,
            Ok(None) => return CheckpointPoll::pending(),
            Err(rejection) => return CheckpointPoll::rejected(rejection),
        };
        let Ok(acknowledgement) = decode_acknowledgement(&frame) else {
            return CheckpointPoll::rejected(CheckpointRejection::Malformed);
        };
        if acknowledgement.nonce != self.nonce {
            return CheckpointPoll::rejected(CheckpointRejection::WrongNonce);
        }
        if self.active_request_id != Some(acknowledgement.request_id) {
            return CheckpointPoll::rejected(CheckpointRejection::Replay);
        }
        self.active_request_id = None;
        self.deadline_at = None;
        self.state = match acknowledgement.status {
            CheckpointWorkerStatus::Completed => {
                CheckpointProtocolState::AcknowledgedUnverifiedDurability
            }
            CheckpointWorkerStatus::Failed => CheckpointProtocolState::WorkerFailed,
            CheckpointWorkerStatus::Cancelled => CheckpointProtocolState::Cancelled,
        };
        CheckpointPoll {
            acknowledgement: Some(acknowledgement),
            rejections: Vec::new(),
        }
    }

    fn receive_frame(&mut self, bytes: &[u8]) -> Result<Option<Vec<u8>>, CheckpointRejection> {
        if self.buffer.len().saturating_add(bytes.len())
            > MAX_CHECKPOINT_FRAME_BYTES + FRAME_HEADER_BYTES
        {
            self.buffer.clear();
            return Err(CheckpointRejection::Oversized);
        }
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() < FRAME_HEADER_BYTES {
            return Ok(None);
        }
        let body_len = u32::from_be_bytes([
            self.buffer[0],
            self.buffer[1],
            self.buffer[2],
            self.buffer[3],
        ]) as usize;
        if body_len > MAX_CHECKPOINT_FRAME_BYTES {
            self.buffer.clear();
            return Err(CheckpointRejection::Oversized);
        }
        let frame_len = FRAME_HEADER_BYTES + body_len;
        if self.buffer.len() < frame_len {
            return Ok(None);
        }
        if self.buffer.len() != frame_len {
            self.buffer.clear();
            return Err(CheckpointRejection::Malformed);
        }
        Ok(Some(std::mem::take(&mut self.buffer)))
    }

    /// Cancel the protocol when the negotiated endpoint exits.
    #[must_use]
    pub fn endpoint_exited(&mut self, _at: Duration) -> CheckpointPoll {
        let mut rejections = Vec::new();
        if !self.buffer.is_empty() {
            rejections.push(CheckpointRejection::Partial);
        }
        rejections.push(CheckpointRejection::EndpointExited);
        self.buffer.clear();
        self.active_request_id = None;
        self.deadline_at = None;
        self.endpoint_exited = true;
        self.state = CheckpointProtocolState::Cancelled;
        CheckpointPoll {
            acknowledgement: None,
            rejections,
        }
    }

    /// Cancel the protocol without interpreting cancellation as worker success.
    #[must_use]
    pub fn cancel(&mut self, _at: Duration) -> CheckpointPoll {
        let mut rejections = Vec::new();
        if !self.buffer.is_empty() {
            rejections.push(CheckpointRejection::Partial);
        }
        self.buffer.clear();
        self.active_request_id = None;
        self.deadline_at = None;
        self.state = CheckpointProtocolState::Cancelled;
        CheckpointPoll {
            acknowledgement: None,
            rejections,
        }
    }

    #[must_use]
    pub const fn state(&self) -> CheckpointProtocolState {
        self.state
    }

    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }
}

/// Child side of a checkpoint socketpair, kept close-on-exec until an explicit guarded launch.
#[derive(Debug)]
pub struct CheckpointWorkerEndpoint {
    stream: UnixStream,
}

impl CheckpointWorkerEndpoint {
    pub(crate) fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

/// Stable class of inherited-channel I/O failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointChannelError {
    CreateFailed,
    ConfigureFailed,
    WriteFailed,
    ReadFailed,
    NegotiationFailed,
}

impl fmt::Display for CheckpointChannelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CreateFailed => "checkpoint channel creation failed",
            Self::ConfigureFailed => "checkpoint channel configuration failed",
            Self::WriteFailed => "checkpoint request write failed",
            Self::ReadFailed => "checkpoint acknowledgement read failed",
            Self::NegotiationFailed => "checkpoint channel negotiation failed",
        })
    }
}

impl Error for CheckpointChannelError {}

/// Request setup can fail either before or during the bounded channel write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointChannelRequestError {
    NotReady,
    Protocol(CheckpointProtocolError),
    Channel(CheckpointChannelError),
}

impl fmt::Display for CheckpointChannelRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReady => formatter.write_str("checkpoint worker is not ready"),
            Self::Protocol(error) => error.fmt(formatter),
            Self::Channel(error) => error.fmt(formatter),
        }
    }
}

impl Error for CheckpointChannelRequestError {}

/// Nonblocking supervisor side of one inherited, nonce-bound checkpoint exchange.
#[derive(Debug)]
pub struct CheckpointChannel {
    stream: UnixStream,
    protocol: CheckpointProtocol,
    nonce: CheckpointNonce,
    negotiation_started: bool,
    ready: bool,
    negotiation_buffer: Vec<u8>,
}

impl CheckpointChannel {
    /// Create the supervisor and child sides of one close-on-exec Unix socketpair.
    ///
    /// # Errors
    ///
    /// Returns a redacted channel error when the socketpair or nonblocking mode cannot be created.
    pub fn pair(
        nonce: CheckpointNonce,
    ) -> Result<(Self, CheckpointWorkerEndpoint), CheckpointChannelError> {
        let (supervisor, worker) =
            UnixStream::pair().map_err(|_| CheckpointChannelError::CreateFailed)?;
        supervisor
            .set_nonblocking(true)
            .map_err(|_| CheckpointChannelError::ConfigureFailed)?;
        Ok((
            Self {
                stream: supervisor,
                protocol: CheckpointProtocol::new(nonce),
                nonce,
                negotiation_started: false,
                ready: false,
                negotiation_buffer: Vec::with_capacity(NEGOTIATION_BODY_BYTES + FRAME_HEADER_BYTES),
            },
            CheckpointWorkerEndpoint { stream: worker },
        ))
    }

    /// Encode and write one bounded request without waiting for descriptor readiness.
    ///
    /// # Errors
    ///
    /// Returns a protocol error for invalid request state or a redacted channel error for a partial,
    /// unavailable, or failed nonblocking write.
    pub fn begin_request(
        &mut self,
        request_id: u64,
        requested_at: Duration,
        deadline_at: Duration,
    ) -> Result<(), CheckpointChannelRequestError> {
        if !self.ready {
            return Err(CheckpointChannelRequestError::NotReady);
        }
        let frame = self
            .protocol
            .begin_request(request_id, requested_at, deadline_at)
            .map_err(CheckpointChannelRequestError::Protocol)?;
        match self.stream.write(&frame) {
            Ok(written) if written == frame.len() => Ok(()),
            Ok(_) | Err(_) => Err(CheckpointChannelRequestError::Channel(
                CheckpointChannelError::WriteFailed,
            )),
        }
    }

    /// Send the versioned hello that starts FD-only worker readiness negotiation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the bounded nonblocking write is not completed atomically.
    pub fn begin_negotiation(&mut self) -> Result<(), CheckpointChannelError> {
        if self.negotiation_started || self.ready {
            return Err(CheckpointChannelError::NegotiationFailed);
        }
        let frame = encode_negotiation(HELLO_KIND, self.nonce);
        match self.stream.write(&frame) {
            Ok(written) if written == frame.len() => {
                self.negotiation_started = true;
                Ok(())
            }
            Ok(_) | Err(_) => Err(CheckpointChannelError::WriteFailed),
        }
    }

    /// Poll the inherited descriptor for a matching readiness frame without blocking.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for EOF, malformed or mismatched readiness, and descriptor failure.
    pub fn poll_ready(&mut self) -> Result<bool, CheckpointChannelError> {
        if self.ready {
            return Ok(true);
        }
        if !self.negotiation_started {
            return Ok(false);
        }
        let mut bytes = [0_u8; NEGOTIATION_BODY_BYTES + FRAME_HEADER_BYTES];
        match self.stream.read(&mut bytes) {
            Ok(0) => return Err(CheckpointChannelError::NegotiationFailed),
            Ok(count) => {
                if self.negotiation_buffer.len().saturating_add(count) > bytes.len() {
                    self.negotiation_buffer.clear();
                    return Err(CheckpointChannelError::NegotiationFailed);
                }
                self.negotiation_buffer.extend_from_slice(&bytes[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(_) => return Err(CheckpointChannelError::ReadFailed),
        }
        if self.negotiation_buffer.len() < NEGOTIATION_BODY_BYTES + FRAME_HEADER_BYTES {
            return Ok(false);
        }
        let nonce = decode_negotiation(&self.negotiation_buffer, READY_KIND)
            .map_err(|_| CheckpointChannelError::NegotiationFailed)?;
        if nonce != self.nonce {
            return Err(CheckpointChannelError::NegotiationFailed);
        }
        self.negotiation_buffer.clear();
        self.ready = true;
        Ok(true)
    }

    /// Poll one bounded descriptor read and never wait for worker progress.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for an unexpected descriptor read failure.
    pub fn poll(&mut self, at: Duration) -> Result<CheckpointPoll, CheckpointChannelError> {
        if self.protocol.state() == CheckpointProtocolState::RequestedUnverified {
            let deadline_poll = self.protocol.ingest(at, &[]);
            if !deadline_poll.rejections.is_empty() {
                return Ok(deadline_poll);
            }
        } else {
            return Ok(CheckpointPoll::pending());
        }
        let mut bytes = [0_u8; MAX_CHECKPOINT_FRAME_BYTES + FRAME_HEADER_BYTES];
        match self.stream.read(&mut bytes) {
            Ok(0) => Ok(self.protocol.endpoint_exited(at)),
            Ok(count) => Ok(self.protocol.ingest(at, &bytes[..count])),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                Ok(CheckpointPoll::pending())
            }
            Err(error) if is_endpoint_exit(&error) => Ok(self.protocol.endpoint_exited(at)),
            Err(_) => Err(CheckpointChannelError::ReadFailed),
        }
    }

    /// Close both channel directions and mark the protocol cancelled.
    ///
    /// # Errors
    ///
    /// Returns a redacted channel error if the socket cannot be shut down.
    pub fn cancel(&mut self, at: Duration) -> Result<(), CheckpointChannelError> {
        let _ = self.protocol.cancel(at);
        self.stream
            .shutdown(Shutdown::Both)
            .map_err(|_| CheckpointChannelError::ConfigureFailed)
    }

    #[must_use]
    pub const fn state(&self) -> CheckpointProtocolState {
        self.protocol.state()
    }
}

fn is_endpoint_exit(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    )
}

/// Validated checkpoint signal and deadline configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointSignalConfig {
    signal: SignalNumber,
    timeout: Duration,
}

impl CheckpointSignalConfig {
    /// Accept only a user-defined signal and a positive timeout.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the signal collides with control signals or timeout is zero.
    pub fn new(
        signal: SignalNumber,
        timeout: Duration,
    ) -> Result<Self, CheckpointSignalConfigError> {
        if timeout.is_zero() {
            return Err(CheckpointSignalConfigError::InvalidTimeout);
        }
        let signal_value = i32::from(signal.get());
        if !matches!(signal_value, libc::SIGUSR1 | libc::SIGUSR2) {
            return Err(CheckpointSignalConfigError::SignalCollision);
        }
        Ok(Self { signal, timeout })
    }

    #[must_use]
    pub const fn signal(self) -> SignalNumber {
        self.signal
    }

    #[must_use]
    pub const fn timeout(self) -> Duration {
        self.timeout
    }
}

/// Invalid checkpoint signal configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointSignalConfigError {
    SignalCollision,
    InvalidTimeout,
}

impl fmt::Display for CheckpointSignalConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SignalCollision => "checkpoint signal collides with process-control behavior",
            Self::InvalidTimeout => "checkpoint timeout must be positive",
        })
    }
}

impl Error for CheckpointSignalConfigError {}

fn encode_negotiation(kind: u8, nonce: CheckpointNonce) -> Vec<u8> {
    let mut body = Vec::with_capacity(NEGOTIATION_BODY_BYTES);
    body.extend_from_slice(&MAGIC);
    body.push(CHECKPOINT_PROTOCOL_VERSION);
    body.push(kind);
    body.extend_from_slice(&nonce.0);
    encode_body(&body)
}

fn decode_negotiation(
    frame: &[u8],
    expected_kind: u8,
) -> Result<CheckpointNonce, CheckpointProtocolError> {
    let body = exact_body(frame, NEGOTIATION_BODY_BYTES)?;
    if body[..4] != MAGIC || body[4] != CHECKPOINT_PROTOCOL_VERSION || body[5] != expected_kind {
        return Err(CheckpointProtocolError::MalformedFrame);
    }
    Ok(CheckpointNonce(copy_array::<32>(&body[6..38])?))
}

fn encode_body(body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + body.len());
    frame.extend_from_slice(
        &u32::try_from(body.len())
            .expect("bounded checkpoint body length fits u32")
            .to_be_bytes(),
    );
    frame.extend_from_slice(body);
    frame
}

fn exact_body(frame: &[u8], expected_body_bytes: usize) -> Result<&[u8], CheckpointProtocolError> {
    if frame.len() != FRAME_HEADER_BYTES + expected_body_bytes {
        return Err(CheckpointProtocolError::MalformedFrame);
    }
    let body_len = u32::from_be_bytes(
        frame[..FRAME_HEADER_BYTES]
            .try_into()
            .map_err(|_| CheckpointProtocolError::MalformedFrame)?,
    ) as usize;
    if body_len != expected_body_bytes || body_len > MAX_CHECKPOINT_FRAME_BYTES {
        return Err(CheckpointProtocolError::MalformedFrame);
    }
    Ok(&frame[FRAME_HEADER_BYTES..])
}

fn decode_acknowledgement(
    frame: &[u8],
) -> Result<CheckpointAcknowledgement, CheckpointProtocolError> {
    let body = exact_body(frame, ACKNOWLEDGEMENT_BODY_BYTES)?;
    if body[..4] != MAGIC
        || body[4] != CHECKPOINT_PROTOCOL_VERSION
        || body[5] != ACKNOWLEDGEMENT_KIND
    {
        return Err(CheckpointProtocolError::MalformedFrame);
    }
    let nonce = CheckpointNonce(copy_array::<32>(&body[6..38])?);
    let request_id = u64::from_be_bytes(copy_array::<8>(&body[38..46])?);
    if request_id == 0 {
        return Err(CheckpointProtocolError::MalformedFrame);
    }
    let status = match body[46] {
        1 => CheckpointWorkerStatus::Completed,
        2 => CheckpointWorkerStatus::Failed,
        3 => CheckpointWorkerStatus::Cancelled,
        _ => return Err(CheckpointProtocolError::MalformedFrame),
    };
    let size_bytes = u64::from_be_bytes(copy_array::<8>(&body[49..57])?);
    let artifact = match (body[47], body[48]) {
        (0, 0) if size_bytes == 0 => None,
        (kind @ 1..=3, has_size @ 0..=1) if has_size == 1 || size_bytes == 0 => {
            Some(CheckpointArtifactMetadata {
                kind: match kind {
                    1 => CheckpointArtifactKind::File,
                    2 => CheckpointArtifactKind::Directory,
                    3 => CheckpointArtifactKind::Opaque,
                    _ => unreachable!("matched artifact kind range"),
                },
                size_bytes: (has_size == 1).then_some(size_bytes),
            })
        }
        _ => return Err(CheckpointProtocolError::MalformedFrame),
    };
    Ok(CheckpointAcknowledgement {
        nonce,
        request_id,
        status,
        artifact,
    })
}

fn copy_array<const N: usize>(bytes: &[u8]) -> Result<[u8; N], CheckpointProtocolError> {
    bytes
        .try_into()
        .map_err(|_| CheckpointProtocolError::MalformedFrame)
}
