use std::collections::VecDeque;
use std::time::Duration;

use crate::{
    Action, ActuationFailure, ActuationKind, CheckpointDisposition, Event, POLICY_CONTRACT_VERSION,
    PolicyMachine, PolicyState, SignalNumber,
};

#[cfg(unix)]
use crate::{
    CheckpointArtifactMetadata, CheckpointChannel, CheckpointChannelRequestError,
    CheckpointEndpoint, CheckpointRejection, CheckpointWorkerStatus, ControlError,
    ControlErrorKind, IdentityTracker, NativeProcessInventory, ObservationFailureKind,
    OwnedProcess, ProcessControlHandle, ProcessIdentity, SignalResult, TrackingFrame,
};

/// Maximum number of recent intervention attempts retained per run.
pub const MAX_INTERVENTION_RECORDS: usize = 64;

/// One concrete platform side effect selected by the policy machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Actuation {
    Checkpoint {
        request_id: u64,
        overshoot_bytes: u64,
        deadline_at: Duration,
    },
    Term {
        checkpoint: CheckpointDisposition,
    },
    Kill,
    ForwardSignal(SignalNumber),
}

impl Actuation {
    /// Return the stable kind used when feeding failures back into policy.
    #[must_use]
    pub const fn kind(&self) -> ActuationKind {
        match self {
            Self::Checkpoint { .. } => ActuationKind::Checkpoint,
            Self::Term { .. } => ActuationKind::Term,
            Self::Kill => ActuationKind::Kill,
            Self::ForwardSignal(_) => ActuationKind::ForwardSignal,
        }
    }
}

/// Successful result of a platform actuation attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActuationOutcome {
    Delivered,
    ProcessMissing,
}

/// Narrow side-effect boundary implemented by a platform adapter or deterministic fake.
pub trait InterventionActuator {
    /// Execute one policy-selected side effect.
    ///
    /// # Errors
    ///
    /// Returns a bounded failure class that the engine feeds back into the policy machine.
    fn execute(
        &mut self,
        requested_at: Duration,
        action: Actuation,
    ) -> Result<ActuationOutcome, ActuationFailure>;
}

/// Borrowed, negotiated resources for endpoint-only checkpoint delivery.
#[cfg(unix)]
#[derive(Debug)]
pub struct CheckpointBinding<'a> {
    channel: &'a mut CheckpointChannel,
    endpoint: CheckpointEndpoint,
    signal: SignalNumber,
}

#[cfg(unix)]
impl<'a> CheckpointBinding<'a> {
    #[must_use]
    pub const fn new(
        channel: &'a mut CheckpointChannel,
        endpoint: CheckpointEndpoint,
        signal: SignalNumber,
    ) -> Self {
        Self {
            channel,
            endpoint,
            signal,
        }
    }
}

/// One nonblocking checkpoint-channel observation for the policy loop.
#[cfg(unix)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointObservation {
    pub event: Option<Event>,
    /// Path-free artifact facts the worker attached to an authenticated completion, present only
    /// alongside a [`Event::CheckpointAck`]: a rejected acknowledgement claims no saved state.
    /// The policy machine never sees these — they are the worker's own report, not a decision
    /// input — so they travel beside the event rather than inside it.
    pub artifact: Option<CheckpointArtifactMetadata>,
    pub rejections: Vec<CheckpointRejection>,
}

/// Unix adapter that maps policy actions to their validated endpoint or process-group target.
#[cfg(unix)]
#[derive(Debug)]
pub struct ProcessInterventionActuator<'a> {
    process: ProcessControlHandle,
    /// The root's exact identity, bound once so a later group scan can be identity-tracked.
    root: Option<ProcessIdentity>,
    process_group: i32,
    checkpoint: Option<CheckpointBinding<'a>>,
}

#[cfg(unix)]
impl<'a> ProcessInterventionActuator<'a> {
    #[must_use]
    pub fn new(process: &OwnedProcess, checkpoint: Option<CheckpointBinding<'a>>) -> Self {
        Self {
            process: process.control_handle(),
            root: NativeProcessInventory::new()
                .inspect(process.root_pid().cast_signed())
                .ok()
                .map(|observation| observation.identity),
            process_group: process.process_group_id(),
            checkpoint,
        }
    }

    /// Poll the inherited checkpoint descriptor once for authenticated worker readiness.
    ///
    /// # Errors
    ///
    /// Returns a typed failure when no channel was configured or negotiation fails.
    pub fn poll_checkpoint_ready(&mut self) -> Result<bool, ActuationFailure> {
        self.checkpoint
            .as_mut()
            .ok_or(ActuationFailure::CheckpointUnavailable)?
            .channel
            .poll_ready()
            .map_err(|_| ActuationFailure::CheckpointUnavailable)
    }

    /// Poll the inherited checkpoint descriptor once without waiting for worker progress.
    ///
    /// # Errors
    ///
    /// Returns a typed failure when no channel was negotiated or descriptor I/O fails.
    pub fn poll_checkpoint(
        &mut self,
        at: Duration,
    ) -> Result<CheckpointObservation, ActuationFailure> {
        let binding = self
            .checkpoint
            .as_mut()
            .ok_or(ActuationFailure::CheckpointUnavailable)?;
        let poll = binding
            .channel
            .poll(at)
            .map_err(|_| ActuationFailure::CheckpointUnavailable)?;
        let (event, artifact) = match poll.acknowledgement {
            Some(acknowledgement)
                if acknowledgement.status == CheckpointWorkerStatus::Completed =>
            {
                (
                    Some(Event::CheckpointAck {
                        at,
                        request_id: acknowledgement.request_id(),
                        authenticated: true,
                    }),
                    acknowledgement.artifact,
                )
            }
            Some(_) => (
                Some(Event::ActuationFailed {
                    at,
                    action: ActuationKind::Checkpoint,
                    failure: ActuationFailure::CheckpointRejected,
                }),
                None,
            ),
            None => (None, None),
        };
        let endpoint_failed = poll.rejections.iter().any(|rejection| {
            matches!(
                rejection,
                CheckpointRejection::EndpointExited | CheckpointRejection::PostExit
            )
        });
        Ok(CheckpointObservation {
            event: event.or_else(|| {
                endpoint_failed.then_some(Event::ActuationFailed {
                    at,
                    action: ActuationKind::Checkpoint,
                    failure: ActuationFailure::CheckpointUnavailable,
                })
            }),
            artifact,
            rejections: poll.rejections,
        })
    }

    /// Check the owned process group without blocking.
    ///
    /// # Errors
    ///
    /// Returns a redacted process-control error for an unexpected system-call failure.
    pub fn owned_group_exists(&self) -> Result<bool, ControlError> {
        self.process.owned_group_exists()
    }

    /// Scan the owned group once, through the same identity tracking the sampler uses.
    ///
    /// Without the root identity bound at construction, or with a scan that cannot run, absence
    /// is not shown and the group is treated as live.
    fn group_has_live_member(&self) -> bool {
        let Some(root) = self.root else {
            return true;
        };
        let Ok(mut tracker) = IdentityTracker::new(root, self.process_group) else {
            return true;
        };
        let Ok(snapshot) = NativeProcessInventory::new().snapshot_for_tracker(&tracker) else {
            return true;
        };
        frame_shows_live_member(&tracker.update(snapshot))
    }

    fn classify_group_signal(
        &self,
        result: Result<SignalResult, ControlError>,
    ) -> Result<ActuationOutcome, ActuationFailure> {
        let result = result.map_err(|error| map_control_error(error.kind()))?;
        map_group_signal_result(result, || self.group_has_live_member())
    }
}

#[cfg(unix)]
impl InterventionActuator for ProcessInterventionActuator<'_> {
    fn execute(
        &mut self,
        requested_at: Duration,
        action: Actuation,
    ) -> Result<ActuationOutcome, ActuationFailure> {
        match action {
            Actuation::Checkpoint {
                request_id,
                deadline_at,
                ..
            } => {
                let binding = self
                    .checkpoint
                    .as_mut()
                    .ok_or(ActuationFailure::CheckpointUnavailable)?;
                binding
                    .channel
                    .begin_request(request_id, requested_at, deadline_at)
                    .map_err(map_checkpoint_request_error)?;
                let result = self
                    .process
                    .signal_checkpoint(&binding.endpoint, binding.signal)
                    .map_err(|error| map_control_error(error.kind()))
                    .and_then(|result| map_signal_result(result, true));
                if result.is_err() {
                    let _ = binding.channel.cancel(requested_at);
                }
                result
            }
            Actuation::Term { .. } => self.classify_group_signal(self.process.terminate_group()),
            Actuation::Kill => self.classify_group_signal(self.process.kill_group()),
            Actuation::ForwardSignal(signal) => {
                self.classify_group_signal(self.process.forward_terminal_signal(signal))
            }
        }
    }
}

#[cfg(unix)]
fn map_checkpoint_request_error(error: CheckpointChannelRequestError) -> ActuationFailure {
    match error {
        CheckpointChannelRequestError::NotReady | CheckpointChannelRequestError::Channel(_) => {
            ActuationFailure::CheckpointUnavailable
        }
        CheckpointChannelRequestError::Protocol(_) => ActuationFailure::CheckpointRejected,
    }
}

#[cfg(unix)]
fn map_control_error(kind: ControlErrorKind) -> ActuationFailure {
    match kind {
        ControlErrorKind::UnsupportedExternalSignal
        | ControlErrorKind::InvalidCheckpointEndpoint => ActuationFailure::InvalidTarget,
        ControlErrorKind::WaitFailed
        | ControlErrorKind::InvalidRootStatus
        | ControlErrorKind::GroupQueryFailed
        | ControlErrorKind::TerminalSignalMonitorUnavailable
        | ControlErrorKind::TerminalSignalMonitorAlreadyInstalled
        | ControlErrorKind::ClientReadyUnavailable
        | ControlErrorKind::SignalFailed => ActuationFailure::SignalFailed,
    }
}

/// Whether one fresh scan of the owned group shows a member a group signal could still reach.
///
/// Only a confirmed disappearance counts toward absence. A member that could not be read, or an
/// enumeration that failed outright, is treated as live, because absence was not shown.
#[cfg(unix)]
fn frame_shows_live_member(frame: &TrackingFrame) -> bool {
    !frame.owned_members.is_empty()
        || frame.observation_failures.iter().any(|failure| {
            failure.kind != ObservationFailureKind::Disappeared || failure.pid.is_none()
        })
}

/// Classify a TERM, KILL or forwarded signal sent to the owned group.
///
/// Darwin refuses a group signal with EPERM, not ESRCH, while every member of the group has
/// exited but is not yet reaped, which is where a worker that is still tearing down leaves it. A
/// refusal is therefore the command having ended, exactly like ESRCH, unless the group still has
/// a live member. Only then is it the delivery failure the policy machine must fail closed on.
#[cfg(unix)]
fn map_group_signal_result(
    result: SignalResult,
    group_has_live_member: impl FnOnce() -> bool,
) -> Result<ActuationOutcome, ActuationFailure> {
    match result {
        SignalResult::PermissionDenied if !group_has_live_member() => {
            Ok(ActuationOutcome::ProcessMissing)
        }
        result => map_signal_result(result, false),
    }
}

#[cfg(unix)]
fn map_signal_result(
    result: SignalResult,
    endpoint_only: bool,
) -> Result<ActuationOutcome, ActuationFailure> {
    match result {
        SignalResult::Delivered => Ok(ActuationOutcome::Delivered),
        SignalResult::ProcessMissing if endpoint_only => Err(ActuationFailure::ProcessMissing),
        SignalResult::ProcessMissing => Ok(ActuationOutcome::ProcessMissing),
        SignalResult::PermissionDenied => Err(ActuationFailure::PermissionDenied),
        SignalResult::Failed => Err(ActuationFailure::SignalFailed),
    }
}

/// Bounded evidence for one platform actuation attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterventionRecord {
    pub requested_at: Duration,
    pub action: Actuation,
    pub result: Result<ActuationOutcome, ActuationFailure>,
    pub baseline_footprint_bytes: Option<u64>,
    pub first_observation_latency: Option<Duration>,
    pub observed_footprint_decrease_latency: Option<Duration>,
    pub group_empty_latency: Option<Duration>,
}

/// Whether the runtime should keep sampling, stop after group exit, or report a terminal error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterventionProgress {
    ObserveUntil { deadline_at: Option<Duration> },
    GroupEmpty,
    TerminalSupervisorError,
}

/// Fixed-capacity intervention evidence plus lossless aggregate counters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterventionEvidence {
    records: VecDeque<InterventionRecord>,
    pub policy_contract_version: u16,
    pub total_attempts: u64,
    pub dropped_records: u64,
    pub maximum_overshoot_bytes: u64,
}

impl Default for InterventionEvidence {
    fn default() -> Self {
        Self {
            records: VecDeque::with_capacity(MAX_INTERVENTION_RECORDS),
            policy_contract_version: POLICY_CONTRACT_VERSION,
            total_attempts: 0,
            dropped_records: 0,
            maximum_overshoot_bytes: 0,
        }
    }
}

impl InterventionEvidence {
    /// Return recent attempts in execution order.
    #[must_use]
    pub const fn records(&self) -> &VecDeque<InterventionRecord> {
        &self.records
    }

    fn record_overshoot(&mut self, bytes: u64) {
        self.maximum_overshoot_bytes = self.maximum_overshoot_bytes.max(bytes);
    }

    fn record_attempt(
        &mut self,
        requested_at: Duration,
        action: Actuation,
        result: Result<ActuationOutcome, ActuationFailure>,
        baseline_footprint_bytes: Option<u64>,
    ) {
        self.total_attempts = self.total_attempts.saturating_add(1);
        if self.records.len() == MAX_INTERVENTION_RECORDS {
            let _ = self.records.pop_front();
            self.dropped_records = self.dropped_records.saturating_add(1);
        }
        self.records.push_back(InterventionRecord {
            requested_at,
            action,
            result,
            baseline_footprint_bytes,
            first_observation_latency: None,
            observed_footprint_decrease_latency: None,
            group_empty_latency: None,
        });
    }

    fn observe(&mut self, observed_at: Duration, footprint_bytes: Option<u64>) {
        for record in &mut self.records {
            if observed_at <= record.requested_at {
                continue;
            }
            let latency = observed_at.saturating_sub(record.requested_at);
            record.first_observation_latency.get_or_insert(latency);
            if record.observed_footprint_decrease_latency.is_none()
                && record
                    .baseline_footprint_bytes
                    .zip(footprint_bytes)
                    .is_some_and(|(baseline, observed)| observed < baseline)
            {
                record.observed_footprint_decrease_latency = Some(latency);
            }
        }
    }

    fn observe_group_empty(&mut self, observed_at: Duration) {
        for record in &mut self.records {
            if observed_at > record.requested_at && record.group_empty_latency.is_none() {
                record.group_empty_latency = Some(observed_at.saturating_sub(record.requested_at));
            }
        }
    }
}

/// Executes only state-machine actions and feeds typed failures back into that same machine.
#[derive(Debug)]
pub struct InterventionEngine<A> {
    policy: PolicyMachine,
    actuator: A,
    evidence: InterventionEvidence,
    last_observed_footprint_bytes: Option<u64>,
}

impl<A: InterventionActuator> InterventionEngine<A> {
    #[must_use]
    pub fn new(policy: PolicyMachine, actuator: A) -> Self {
        Self {
            policy,
            actuator,
            evidence: InterventionEvidence::default(),
            last_observed_footprint_bytes: None,
        }
    }

    #[must_use]
    pub const fn policy(&self) -> &PolicyMachine {
        &self.policy
    }

    #[must_use]
    pub const fn actuator(&self) -> &A {
        &self.actuator
    }

    pub const fn actuator_mut(&mut self) -> &mut A {
        &mut self.actuator
    }

    #[must_use]
    pub const fn evidence(&self) -> &InterventionEvidence {
        &self.evidence
    }

    /// Record one nonblocking group-status observation and choose the loop's next condition.
    pub fn observe_group_status(
        &mut self,
        observed_at: Duration,
        group_exists: bool,
    ) -> InterventionProgress {
        if !group_exists {
            self.evidence.observe_group_empty(observed_at);
            return InterventionProgress::GroupEmpty;
        }
        if self.policy.state() == PolicyState::SupervisorError
            && self.policy.next_deadline().is_none()
        {
            return InterventionProgress::TerminalSupervisorError;
        }
        InterventionProgress::ObserveUntil {
            deadline_at: self.policy.next_deadline(),
        }
    }

    /// Apply one ordered event, execute its policy-selected effects, and return all decisions.
    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        self.handle_with_transition_observer(event, |_, _, _| {})
    }

    /// Apply one ordered event while exposing every state change before its selected actuation.
    ///
    /// The observer is called after the pure policy decision and before any corresponding platform
    /// side effect. Runtimes use this boundary to persist decisive transitions before signalling.
    pub fn handle_with_transition_observer<F>(
        &mut self,
        event: Event,
        mut observe_transition: F,
    ) -> Vec<Action>
    where
        F: FnMut(Duration, PolicyState, PolicyState),
    {
        let at = event.at();
        let sample_footprint = match &event {
            Event::Sample(sample) => Some(sample.aggregate_bytes),
            _ => None,
        };
        let previous_state = self.policy.state();
        let initial = self.policy.apply(event);
        let current_state = self.policy.state();
        if previous_state != current_state {
            observe_transition(at, previous_state, current_state);
        }
        if initial.contains(&Action::RecordObservation)
            && let Some(footprint_bytes) = sample_footprint
        {
            self.evidence.observe(at, footprint_bytes);
            self.last_observed_footprint_bytes = footprint_bytes;
        }

        let mut pending = VecDeque::from(initial);
        let mut decisions = Vec::with_capacity(pending.len());
        while let Some(action) = pending.pop_front() {
            match action {
                Action::RecordOvershoot { bytes } => self.evidence.record_overshoot(bytes),
                Action::RequestCheckpoint {
                    overshoot_bytes, ..
                } => self.evidence.record_overshoot(overshoot_bytes),
                _ => {}
            }
            decisions.push(action);

            let Some(actuation) = actuation_for(action) else {
                continue;
            };
            let kind = actuation.kind();
            let result = self.actuator.execute(at, actuation);
            self.evidence
                .record_attempt(at, actuation, result, self.last_observed_footprint_bytes);
            if let Err(failure) = result {
                let previous_state = self.policy.state();
                let after_failure = self.policy.apply(Event::ActuationFailed {
                    at,
                    action: kind,
                    failure,
                });
                let current_state = self.policy.state();
                if previous_state != current_state {
                    observe_transition(at, previous_state, current_state);
                }
                pending.extend(after_failure);
            }
        }
        decisions
    }
}

fn actuation_for(action: Action) -> Option<Actuation> {
    match action {
        Action::RequestCheckpoint {
            request_id,
            overshoot_bytes,
            deadline_at,
        } => Some(Actuation::Checkpoint {
            request_id,
            overshoot_bytes,
            deadline_at,
        }),
        Action::SendTerm { checkpoint } => Some(Actuation::Term { checkpoint }),
        Action::SendKill => Some(Actuation::Kill),
        Action::ForwardSignal(signal) => Some(Actuation::ForwardSignal(signal)),
        Action::RecordObservation
        | Action::RecordMissing
        | Action::RecordOvershoot { .. }
        | Action::StopObserving
        | Action::ReportExit { .. }
        | Action::ReportSupervisorError { .. } => None,
    }
}

// These live in the crate because no external caller can play the worker: `raw_fd` is
// `pub(crate)` and no fixture mode ever acknowledges anything but `Completed`, so a rejected
// acknowledgement carrying artifact facts has no integration-test representation.
#[cfg(all(test, unix))]
#[allow(unsafe_code)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::mem::ManuallyDrop;
    use std::os::fd::FromRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    #[cfg(target_os = "macos")]
    use super::{Actuation, InterventionActuator};
    use super::{
        ActuationFailure, ActuationKind, ActuationOutcome, CheckpointBinding,
        CheckpointObservation, Event, ProcessInterventionActuator, frame_shows_live_member,
        map_group_signal_result,
    };
    use crate::{
        AggregateFootprint, CheckpointAcknowledgement, CheckpointArtifactKind,
        CheckpointArtifactMetadata, CheckpointChannel, CheckpointHello, CheckpointNonce,
        CheckpointRequest, CheckpointWorkerStatus, LaunchOptions, NativeProcessInventory,
        ObservationFailure, ObservationFailureKind, OwnedProcess, ProcessIdentity,
        ProcessObservation, SignalResult, StdioMode, TrackingFrame, checkpoint_signal_usr1,
    };
    #[cfg(target_os = "macos")]
    use crate::{CheckpointDisposition, SignalNumber};

    /// 4-byte length header plus the 38-byte negotiation body.
    const HELLO_FRAME_BYTES: usize = 42;
    /// 4-byte length header plus the 54-byte request body.
    const REQUEST_FRAME_BYTES: usize = 58;
    const NONCE: CheckpointNonce = CheckpointNonce::from_bytes([7; 32]);
    const REQUEST_ID: u64 = 11;
    /// Generous enough that a starved runner never trips it, short enough to fail rather than hang.
    const READ_TIMEOUT: Duration = Duration::from_secs(5);
    /// The worker's own claim about saved state, distinctive enough that leaking it is unmistakable.
    const ARTIFACT: CheckpointArtifactMetadata = CheckpointArtifactMetadata {
        kind: CheckpointArtifactKind::Directory,
        size_bytes: Some(4_096),
    };

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    /// Launch one inert, self-limiting child purely to mint a validated owned group.
    ///
    /// `poll_checkpoint` reads the inherited descriptor and never signals, so this process is only
    /// here to supply the `ProcessControlHandle` and `CheckpointEndpoint` the actuator is built
    /// from. It is killed and reaped when the returned value drops.
    fn inert_root() -> OwnedProcess {
        OwnedProcess::launch(&LaunchOptions {
            command: vec![OsString::from("/bin/sleep"), OsString::from("10")],
            cwd: None,
            clear_env: false,
            env: BTreeMap::new(),
            stdin: StdioMode::Null,
            stdout: StdioMode::Null,
            stderr: StdioMode::Null,
        })
        .expect("/bin/sleep launches into its own process group")
    }

    fn root_identity(process: &OwnedProcess) -> ProcessIdentity {
        NativeProcessInventory::new()
            .inspect(process.root_pid().cast_signed())
            .expect("launched root process is inspectable")
            .identity
    }

    /// Run one full cooperative exchange whose acknowledgement carries `ARTIFACT`, and return what
    /// `poll_checkpoint` made of it.
    ///
    /// The worker is this test: the request is written straight onto the channel rather than
    /// through `Actuation::Checkpoint`, so no signal is ever delivered and the exchange stays
    /// deterministic.
    fn observe_acknowledgement(
        process: &OwnedProcess,
        status: CheckpointWorkerStatus,
    ) -> CheckpointObservation {
        let (mut channel, inherited) =
            CheckpointChannel::pair(NONCE).expect("socketpair creation succeeds");
        let inherited = ManuallyDrop::new(inherited);
        // SAFETY: `raw_fd` hands back the live worker descriptor. The `ManuallyDrop` above means
        // the endpoint never closes it, so this stream is its sole owner and closes it once.
        let mut worker = unsafe { UnixStream::from_raw_fd(inherited.raw_fd()) };
        // Both reads below demand an exact frame length from a blocking socket, and `cargo test`
        // has no per-test timeout. Without this bound, a changed body size would hang the runner
        // for its whole job budget instead of failing; with it, the read names the frame it wanted.
        worker
            .set_read_timeout(Some(READ_TIMEOUT))
            .expect("a positive read timeout is accepted");

        channel.begin_negotiation().expect("hello is written");
        let mut hello = [0_u8; HELLO_FRAME_BYTES];
        worker.read_exact(&mut hello).expect("hello frame arrives");
        let ready = CheckpointHello::decode(&hello)
            .expect("supervisor hello decodes")
            .ready_frame();
        worker.write_all(&ready).expect("readiness is written");
        assert!(channel.poll_ready().expect("readiness frame decodes"));

        channel
            .begin_request(REQUEST_ID, ms(0), ms(50))
            .expect("request is written");
        let mut frame = [0_u8; REQUEST_FRAME_BYTES];
        worker
            .read_exact(&mut frame)
            .expect("request frame arrives");
        let request = CheckpointRequest::decode(&frame).expect("request decodes");
        assert_eq!(request.request_id(), REQUEST_ID);
        let acknowledgement =
            CheckpointAcknowledgement::for_request(&request, status, Some(ARTIFACT));
        assert_eq!(acknowledgement.artifact, Some(ARTIFACT));
        worker
            .write_all(&acknowledgement.encode())
            .expect("acknowledgement is written");

        let endpoint = process
            .negotiate_checkpoint_endpoint(root_identity(process))
            .expect("the launched root is its own group leader");
        let binding = CheckpointBinding::new(&mut channel, endpoint, checkpoint_signal_usr1());
        let mut actuator = ProcessInterventionActuator::new(process, Some(binding));

        // One socketpair write is delivered whole, but a short read would only look like "nothing
        // yet", so poll until the channel resolves rather than trusting a single attempt.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let observation = actuator
                .poll_checkpoint(ms(20))
                .expect("the inherited descriptor is readable");
            if observation.event.is_some() || !observation.rejections.is_empty() {
                return observation;
            }
            assert!(
                Instant::now() < deadline,
                "the acknowledgement never reached the supervisor"
            );
        }
    }

    fn identity(pid: i32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_abstime: 1_000,
        }
    }

    fn live_member(pid: i32) -> ProcessObservation {
        ProcessObservation {
            identity: identity(pid),
            parent_pid: 1,
            process_group_id: 100,
            footprint_bytes: Some(4_096),
            exited: false,
        }
    }

    fn frame(
        owned_members: Vec<ProcessObservation>,
        observation_failures: Vec<ObservationFailure>,
    ) -> TrackingFrame {
        TrackingFrame {
            owned_members,
            escape_observed: false,
            events: Vec::new(),
            aggregate_footprint: AggregateFootprint::Complete(0),
            observation_failures,
            root_exit_observed: true,
        }
    }

    fn failure(pid: Option<i32>, kind: ObservationFailureKind) -> ObservationFailure {
        ObservationFailure { pid, kind }
    }

    #[test]
    fn a_refused_group_signal_with_no_live_member_means_the_command_ended() {
        // Catches EPERM from a group of exited, unreaped members staying a delivery failure, which
        // turns an intervention that worked into exit 70.
        assert_eq!(
            map_group_signal_result(SignalResult::PermissionDenied, || false),
            Ok(ActuationOutcome::ProcessMissing)
        );
    }

    #[test]
    fn a_refused_group_signal_with_a_live_member_stays_a_failure() {
        // Catches the "ended" reading swallowing a real refusal: a live member the supervisor may
        // not signal is exactly the failure exit 70 exists for.
        assert_eq!(
            map_group_signal_result(SignalResult::PermissionDenied, || true),
            Err(ActuationFailure::PermissionDenied)
        );
    }

    #[test]
    fn group_results_other_than_a_refusal_keep_their_meaning_whatever_the_liveness() {
        // Catches the liveness reading leaking past EPERM: a delivered signal must stay delivered
        // and an unexpected errno must stay a failure even for a group with no live member.
        for live in [false, true] {
            assert_eq!(
                map_group_signal_result(SignalResult::Delivered, || live),
                Ok(ActuationOutcome::Delivered)
            );
            assert_eq!(
                map_group_signal_result(SignalResult::ProcessMissing, || live),
                Ok(ActuationOutcome::ProcessMissing)
            );
            assert_eq!(
                map_group_signal_result(SignalResult::Failed, || live),
                Err(ActuationFailure::SignalFailed)
            );
        }
    }

    #[test]
    fn a_group_with_only_confirmed_exits_has_no_live_member() {
        // Catches a reaped root (its confirmed disappearance) being read as a live member, which
        // would leave every refused signal to an ended group a supervisor failure.
        assert!(!frame_shows_live_member(&frame(Vec::new(), Vec::new())));
        assert!(!frame_shows_live_member(&frame(
            Vec::new(),
            vec![failure(Some(42), ObservationFailureKind::Disappeared)]
        )));
    }

    #[test]
    fn a_live_member_or_an_unreadable_one_keeps_the_group_alive() {
        // Catches proof of absence being claimed without it: a live member, a member that could
        // not be read, or an enumeration that failed outright all leave the refusal a failure.
        assert!(frame_shows_live_member(&frame(
            vec![live_member(42)],
            Vec::new()
        )));
        for kind in [
            ObservationFailureKind::PermissionDenied,
            ObservationFailureKind::StaleIdentity,
            ObservationFailureKind::MalformedData,
            ObservationFailureKind::Unsupported,
            ObservationFailureKind::EnumerationFailed,
            ObservationFailureKind::Unavailable,
        ] {
            assert!(
                frame_shows_live_member(&frame(Vec::new(), vec![failure(Some(42), kind)])),
                "{kind:?}"
            );
        }
        assert!(frame_shows_live_member(&frame(
            Vec::new(),
            vec![failure(None, ObservationFailureKind::Disappeared)]
        )));
    }

    /// Launch a root that exits at once and return it after it has exited but before any reap.
    #[cfg(target_os = "macos")]
    fn exited_unreaped_root() -> OwnedProcess {
        let process = OwnedProcess::launch(&LaunchOptions {
            command: vec![OsString::from("/usr/bin/true")],
            cwd: None,
            clear_env: false,
            env: BTreeMap::new(),
            stdin: StdioMode::Null,
            stdout: StdioMode::Null,
            stderr: StdioMode::Null,
        })
        .expect("/usr/bin/true launches into its own process group");
        let pid = process.root_pid().cast_signed();
        let deadline = Instant::now() + READ_TIMEOUT;
        while !NativeProcessInventory::new()
            .inspect(pid)
            .is_ok_and(|observation| observation.exited)
        {
            assert!(Instant::now() < deadline, "the root never exited");
            std::thread::sleep(Duration::from_millis(1));
        }
        process
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn term_and_kill_to_an_exited_unreaped_group_report_it_missing() {
        // Catches the actuator handing the platform's EPERM for an ended group straight to policy,
        // for TERM, KILL and a forwarded terminal signal alike.
        let process = exited_unreaped_root();
        // The premise this classification exists for, measured on Darwin: a group whose only
        // member has exited but is not yet reaped refuses the signal instead of reporting ESRCH.
        assert!(matches!(
            process.terminate_group(),
            Ok(SignalResult::PermissionDenied)
        ));
        let mut actuator = ProcessInterventionActuator::new(&process, None);

        let term = actuator.execute(
            ms(0),
            Actuation::Term {
                checkpoint: CheckpointDisposition::SkippedNotNegotiated,
            },
        );
        let kill = actuator.execute(ms(0), Actuation::Kill);
        // A terminal signal forwarded from the supervisor goes through the same classification.
        let forwarded = actuator.execute(
            ms(0),
            Actuation::ForwardSignal(SignalNumber::new(15).expect("15 is a valid signal number")),
        );

        assert_eq!(term, Ok(ActuationOutcome::ProcessMissing));
        assert_eq!(kill, Ok(ActuationOutcome::ProcessMissing));
        assert_eq!(forwarded, Ok(ActuationOutcome::ProcessMissing));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_running_group_reads_as_live_and_an_ended_one_does_not() {
        // Catches the liveness scan missing a running root (which would excuse a real refusal) or
        // counting an exited, unreaped one (which would leave the ended group a failure).
        let running = inert_root();
        let ended = exited_unreaped_root();

        assert!(ProcessInterventionActuator::new(&running, None).group_has_live_member());
        assert!(!ProcessInterventionActuator::new(&ended, None).group_has_live_member());
    }

    #[test]
    fn rejected_acknowledgement_drops_the_artifact_the_worker_claimed() {
        // Catches `poll_checkpoint` forwarding acknowledgement artifact facts on any status
        // instead of only on `Completed`, which would let a failed checkpoint publish a
        // resume artifact the worker never saved.
        let process = inert_root();

        let observation = observe_acknowledgement(&process, CheckpointWorkerStatus::Failed);

        assert_eq!(observation.artifact, None);
        assert_eq!(
            observation.event,
            Some(Event::ActuationFailed {
                at: ms(20),
                action: ActuationKind::Checkpoint,
                failure: ActuationFailure::CheckpointRejected,
            })
        );
        assert!(observation.rejections.is_empty());
    }

    #[test]
    fn completed_acknowledgement_carries_the_artifact_through() {
        // Positive control for the test above: catches an artifact that never reaches the
        // supervisor at all, which would make the rejected-acknowledgement pin vacuous.
        let process = inert_root();

        let observation = observe_acknowledgement(&process, CheckpointWorkerStatus::Completed);

        assert_eq!(observation.artifact, Some(ARTIFACT));
        assert_eq!(
            observation.event,
            Some(Event::CheckpointAck {
                at: ms(20),
                request_id: REQUEST_ID,
                authenticated: true,
            })
        );
        assert!(observation.rejections.is_empty());
    }
}
