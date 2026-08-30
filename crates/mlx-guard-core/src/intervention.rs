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
    ControlErrorKind, OwnedProcess, ProcessControlHandle, SignalResult,
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
    checkpoint: Option<CheckpointBinding<'a>>,
}

#[cfg(unix)]
impl<'a> ProcessInterventionActuator<'a> {
    #[must_use]
    pub fn new(process: &OwnedProcess, checkpoint: Option<CheckpointBinding<'a>>) -> Self {
        Self {
            process: process.control_handle(),
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
            Actuation::Term { .. } => self
                .process
                .terminate_group()
                .map_err(|error| map_control_error(error.kind()))
                .and_then(|result| map_signal_result(result, false)),
            Actuation::Kill => self
                .process
                .kill_group()
                .map_err(|error| map_control_error(error.kind()))
                .and_then(|result| map_signal_result(result, false)),
            Actuation::ForwardSignal(signal) => self
                .process
                .forward_terminal_signal(signal)
                .map_err(|error| map_control_error(error.kind()))
                .and_then(|result| map_signal_result(result, false)),
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

    use super::{
        ActuationFailure, ActuationKind, CheckpointBinding, CheckpointObservation, Event,
        ProcessInterventionActuator,
    };
    use crate::{
        CheckpointAcknowledgement, CheckpointArtifactKind, CheckpointArtifactMetadata,
        CheckpointChannel, CheckpointHello, CheckpointNonce, CheckpointRequest,
        CheckpointWorkerStatus, LaunchOptions, NativeProcessInventory, OwnedProcess,
        ProcessIdentity, StdioMode, checkpoint_signal_usr1,
    };

    /// 4-byte length header plus the 38-byte negotiation body.
    const HELLO_FRAME_BYTES: usize = 42;
    /// 4-byte length header plus the 54-byte request body.
    const REQUEST_FRAME_BYTES: usize = 58;
    const NONCE: CheckpointNonce = CheckpointNonce::from_bytes([7; 32]);
    const REQUEST_ID: u64 = 11;
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
