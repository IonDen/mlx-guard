use std::collections::VecDeque;
use std::time::Duration;

use crate::{
    Action, ActuationFailure, ActuationKind, CheckpointDisposition, Event, POLICY_CONTRACT_VERSION,
    PolicyMachine, PolicyState, SignalNumber,
};

#[cfg(unix)]
use crate::{
    CheckpointChannel, CheckpointChannelRequestError, CheckpointEndpoint, CheckpointRejection,
    CheckpointWorkerStatus, ControlError, ControlErrorKind, OwnedProcess, ProcessControlHandle,
    SignalResult,
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
        let event = poll.acknowledgement.map(|acknowledgement| {
            if acknowledgement.status == CheckpointWorkerStatus::Completed {
                Event::CheckpointAck {
                    at,
                    request_id: acknowledgement.request_id(),
                    authenticated: true,
                }
            } else {
                Event::ActuationFailed {
                    at,
                    action: ActuationKind::Checkpoint,
                    failure: ActuationFailure::CheckpointRejected,
                }
            }
        });
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
