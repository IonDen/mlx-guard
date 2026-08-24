use std::error::Error;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::SignalNumber;

/// Stable version of the public state-machine decision contract.
pub const POLICY_CONTRACT_VERSION: u16 = 1;

/// Configuration for the pure enforcement state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyConfig {
    pub limit_bytes: u64,
    pub warning_bytes: u64,
    pub recovery_bytes: u64,
    pub emergency_bytes: u64,
    pub required_breach_samples: u32,
    pub max_missing_samples: u32,
    pub max_sample_age: Duration,
    pub max_sample_window: Duration,
    pub checkpoint_timeout: Option<Duration>,
    pub term_grace: Duration,
    pub wall_time: Option<Duration>,
}

/// A rejected policy configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyConfigError;

impl fmt::Display for PolicyConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("policy thresholds or durations are invalid")
    }
}

impl Error for PolicyConfigError {}

/// The externally visible phase of policy evaluation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyState {
    Observe,
    Normal,
    Warning,
    CheckpointRequested,
    Terminating,
    Emergency,
    Exited,
    SupervisorError,
}

/// What happened before the supervisor sent the graceful termination signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointDisposition {
    AcknowledgedUnverifiedDurability,
    TimedOut,
    SkippedCheckpointFailure,
    SkippedObservationFailure,
    SkippedSupervisorFailure,
    SkippedNotNegotiated,
    SkippedRootExited,
    SkippedParentExited,
}

/// The platform side effect whose execution failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActuationKind {
    Checkpoint,
    Term,
    Kill,
    ForwardSignal,
}

/// A bounded, non-sensitive classification of a platform actuation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActuationFailure {
    CheckpointUnavailable,
    CheckpointRejected,
    ProcessMissing,
    PermissionDenied,
    InvalidTarget,
    SignalFailed,
}

/// A measurement delivered to the policy machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SampleEvent {
    pub captured_at: Duration,
    pub processed_at: Duration,
    pub window: Duration,
    pub aggregate_bytes: Option<u64>,
}

/// An ordered input to the policy machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Sample(SampleEvent),
    Tick {
        at: Duration,
    },
    CheckpointAck {
        at: Duration,
        request_id: u64,
        authenticated: bool,
    },
    ExternalSignal {
        at: Duration,
        signal: SignalNumber,
    },
    ActuationFailed {
        at: Duration,
        action: ActuationKind,
        failure: ActuationFailure,
    },
    SupervisorFault {
        at: Duration,
    },
    ProcessExited {
        at: Duration,
        final_footprint_bytes: Option<u64>,
    },
    RootExited {
        at: Duration,
    },
    ParentExited {
        at: Duration,
    },
}

impl Event {
    pub(crate) fn at(&self) -> Duration {
        match self {
            Self::Sample(sample) => sample.processed_at,
            Self::Tick { at }
            | Self::CheckpointAck { at, .. }
            | Self::ExternalSignal { at, .. }
            | Self::ActuationFailed { at, .. }
            | Self::SupervisorFault { at }
            | Self::ProcessExited { at, .. }
            | Self::RootExited { at }
            | Self::ParentExited { at } => *at,
        }
    }
}

/// A side effect requested from the platform supervisor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    RecordObservation,
    RecordMissing,
    RequestCheckpoint {
        request_id: u64,
        overshoot_bytes: u64,
        deadline_at: Duration,
    },
    SendTerm {
        checkpoint: CheckpointDisposition,
    },
    SendKill,
    RecordOvershoot {
        bytes: u64,
    },
    StopObserving,
    ForwardSignal(SignalNumber),
    ReportExit {
        final_footprint_bytes: Option<u64>,
        post_signal_observations: u32,
    },
    ReportSupervisorError {
        action: ActuationKind,
        failure: ActuationFailure,
    },
}

impl Action {
    /// Return whether this action asks the platform layer to deliver a signal.
    #[must_use]
    pub const fn is_signal(&self) -> bool {
        matches!(
            self,
            Self::SendTerm { .. } | Self::SendKill | Self::ForwardSignal(_)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PolicyMode {
    Observe,
    Enforce,
}

/// Pure transition function for observation and enforcement policy.
#[derive(Clone, Debug)]
pub struct PolicyMachine {
    mode: PolicyMode,
    state: PolicyState,
    config: PolicyConfig,
    breach_streak: u32,
    missing_streak: u32,
    next_request_id: u64,
    active_request_id: Option<u64>,
    checkpoint_deadline: Option<Duration>,
    term_deadline: Option<Duration>,
    last_event_at: Option<Duration>,
    post_signal_observations: u32,
    intervention_started: bool,
}

impl PolicyMachine {
    /// Create a non-destructive observer with explicit measurement quality limits.
    #[must_use]
    pub fn observe(
        max_sample_age: Duration,
        max_sample_window: Duration,
        max_missing_samples: u32,
    ) -> Self {
        Self {
            mode: PolicyMode::Observe,
            state: PolicyState::Observe,
            config: PolicyConfig {
                limit_bytes: u64::MAX,
                warning_bytes: u64::MAX - 1,
                recovery_bytes: u64::MAX - 2,
                emergency_bytes: u64::MAX,
                required_breach_samples: u32::MAX,
                max_missing_samples,
                max_sample_age,
                max_sample_window,
                checkpoint_timeout: None,
                term_grace: Duration::MAX,
                wall_time: None,
            },
            breach_streak: 0,
            missing_streak: 0,
            next_request_id: 1,
            active_request_id: None,
            checkpoint_deadline: None,
            term_deadline: None,
            last_event_at: None,
            post_signal_observations: 0,
            intervention_started: false,
        }
    }

    /// Create an enforcing machine after validating all ordering constraints.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyConfigError`] when thresholds are unordered, counts are zero, or a
    /// configured duration is zero.
    pub fn enforce(config: PolicyConfig) -> Result<Self, PolicyConfigError> {
        Self::enforce_with_initial_request_id(config, 1)
    }

    /// Create an enforcing machine with a nonzero, per-run checkpoint request seed.
    ///
    /// Runtime callers should use an unpredictable seed so an endpoint cannot queue a valid
    /// acknowledgement before the corresponding request exists. [`Self::enforce`] remains
    /// deterministic for pure policy users and tests.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyConfigError`] when the policy is invalid or `initial_request_id` is zero.
    pub fn enforce_with_initial_request_id(
        config: PolicyConfig,
        initial_request_id: u64,
    ) -> Result<Self, PolicyConfigError> {
        if !Self::valid_config(&config) || initial_request_id == 0 {
            return Err(PolicyConfigError);
        }
        Ok(Self {
            mode: PolicyMode::Enforce,
            state: PolicyState::Normal,
            config,
            breach_streak: 0,
            missing_streak: 0,
            next_request_id: initial_request_id,
            active_request_id: None,
            checkpoint_deadline: None,
            term_deadline: None,
            last_event_at: None,
            post_signal_observations: 0,
            intervention_started: false,
        })
    }

    /// Return the current state without exposing mutable internals.
    #[must_use]
    pub const fn state(&self) -> PolicyState {
        self.state
    }

    /// Return the stable decision-contract version used by this machine.
    #[must_use]
    pub const fn contract_version(&self) -> u16 {
        POLICY_CONTRACT_VERSION
    }

    /// Return the next state-machine deadline that the runtime must schedule.
    #[must_use]
    pub const fn next_deadline(&self) -> Option<Duration> {
        match self.state {
            PolicyState::CheckpointRequested => self.checkpoint_deadline,
            PolicyState::Terminating | PolicyState::SupervisorError => self.term_deadline,
            PolicyState::Observe
            | PolicyState::Normal
            | PolicyState::Warning
            | PolicyState::Emergency
            | PolicyState::Exited => None,
        }
    }

    /// Apply one ordered event and return side effects in execution order.
    pub fn apply(&mut self, event: Event) -> Vec<Action> {
        if self.state == PolicyState::Exited {
            return Vec::new();
        }

        if let Event::ProcessExited {
            final_footprint_bytes,
            ..
        } = event
        {
            self.state = PolicyState::Exited;
            return vec![Action::ReportExit {
                final_footprint_bytes,
                post_signal_observations: self.post_signal_observations,
            }];
        }

        let at = event.at();
        if self.last_event_at.is_some_and(|previous| at < previous) {
            return self.fail_clock();
        }
        self.last_event_at = Some(at);

        match event {
            Event::Sample(sample) => self.apply_sample(&sample),
            Event::Tick { at } => self.apply_tick(at),
            Event::CheckpointAck {
                at,
                request_id,
                authenticated,
            } => self.apply_checkpoint_ack(at, request_id, authenticated),
            Event::ExternalSignal { at, signal } => self.apply_external_signal(at, signal),
            Event::ActuationFailed {
                at,
                action,
                failure,
            } => self.apply_actuation_failed(at, action, failure),
            Event::SupervisorFault { at } => self.apply_supervisor_fault(at),
            Event::RootExited { at } => self.apply_root_exited(at),
            Event::ParentExited { at } => self.apply_parent_exited(at),
            Event::ProcessExited { .. } => unreachable!("process exit handled before dispatch"),
        }
    }

    fn valid_config(config: &PolicyConfig) -> bool {
        config.recovery_bytes < config.warning_bytes
            && config.warning_bytes < config.limit_bytes
            && config.limit_bytes < config.emergency_bytes
            && config.required_breach_samples > 0
            && config.max_missing_samples > 0
            && !config.max_sample_age.is_zero()
            && !config.max_sample_window.is_zero()
            && !config.term_grace.is_zero()
            && config
                .checkpoint_timeout
                .is_none_or(|timeout| !timeout.is_zero())
            && config.wall_time.is_none_or(|limit| !limit.is_zero())
    }

    fn apply_sample(&mut self, sample: &SampleEvent) -> Vec<Action> {
        let age = sample.processed_at.saturating_sub(sample.captured_at);
        let valid = sample.aggregate_bytes.is_some()
            && sample.processed_at >= sample.captured_at
            && age <= self.config.max_sample_age
            && sample.window <= self.config.max_sample_window;
        if !valid {
            return self.apply_missing(sample.processed_at);
        }

        self.missing_streak = 0;
        if self.intervention_started {
            self.post_signal_observations = self.post_signal_observations.saturating_add(1);
        }
        if self.mode == PolicyMode::Observe || self.intervention_started {
            return vec![Action::RecordObservation];
        }

        let bytes = sample
            .aggregate_bytes
            .expect("validated aggregate is present");
        if bytes >= self.config.emergency_bytes {
            self.state = PolicyState::Emergency;
            self.intervention_started = true;
            return vec![
                Action::RecordObservation,
                Action::SendKill,
                Action::RecordOvershoot {
                    bytes: bytes.saturating_sub(self.config.limit_bytes),
                },
            ];
        }

        if self.state == PolicyState::CheckpointRequested {
            return vec![Action::RecordObservation];
        }

        if bytes >= self.config.limit_bytes {
            self.breach_streak = self.breach_streak.saturating_add(1);
            self.state = PolicyState::Warning;
            let mut actions = vec![Action::RecordObservation];
            if self.breach_streak >= self.config.required_breach_samples {
                let overshoot_bytes = bytes.saturating_sub(self.config.limit_bytes);
                actions.extend(self.begin_graceful(
                    sample.processed_at,
                    overshoot_bytes,
                    CheckpointDisposition::SkippedNotNegotiated,
                ));
                if self.config.checkpoint_timeout.is_none() {
                    actions.push(Action::RecordOvershoot {
                        bytes: overshoot_bytes,
                    });
                }
            }
            return actions;
        }

        self.breach_streak = 0;
        if bytes >= self.config.warning_bytes {
            self.state = PolicyState::Warning;
        } else if bytes <= self.config.recovery_bytes {
            self.state = PolicyState::Normal;
        }
        vec![Action::RecordObservation]
    }

    fn apply_missing(&mut self, at: Duration) -> Vec<Action> {
        self.missing_streak = self.missing_streak.saturating_add(1);
        let mut actions = vec![Action::RecordMissing];
        if self.missing_streak < self.config.max_missing_samples
            || self.state == PolicyState::SupervisorError
            || self.intervention_started
        {
            return actions;
        }

        self.state = PolicyState::SupervisorError;
        match self.mode {
            PolicyMode::Observe => actions.push(Action::StopObserving),
            PolicyMode::Enforce => {
                self.intervention_started = true;
                self.term_deadline = Some(at.saturating_add(self.config.term_grace));
                actions.push(Action::SendTerm {
                    checkpoint: CheckpointDisposition::SkippedObservationFailure,
                });
            }
        }
        actions
    }

    fn begin_graceful(
        &mut self,
        at: Duration,
        overshoot_bytes: u64,
        skipped: CheckpointDisposition,
    ) -> Vec<Action> {
        if let Some(timeout) = self.config.checkpoint_timeout {
            let request_id = self.next_request_id;
            let deadline_at = at.saturating_add(timeout);
            self.next_request_id = self.next_request_id.saturating_add(1);
            self.active_request_id = Some(request_id);
            self.checkpoint_deadline = Some(deadline_at);
            self.state = PolicyState::CheckpointRequested;
            vec![Action::RequestCheckpoint {
                request_id,
                overshoot_bytes,
                deadline_at,
            }]
        } else {
            self.state = PolicyState::Terminating;
            self.intervention_started = true;
            self.term_deadline = Some(at.saturating_add(self.config.term_grace));
            vec![Action::SendTerm {
                checkpoint: skipped,
            }]
        }
    }

    fn apply_checkpoint_ack(
        &mut self,
        at: Duration,
        request_id: u64,
        authenticated: bool,
    ) -> Vec<Action> {
        if self.state != PolicyState::CheckpointRequested
            || self.active_request_id != Some(request_id)
            || !authenticated
        {
            return Vec::new();
        }
        self.active_request_id = None;
        self.checkpoint_deadline = None;
        self.state = PolicyState::Terminating;
        self.intervention_started = true;
        self.term_deadline = Some(at.saturating_add(self.config.term_grace));
        vec![Action::SendTerm {
            checkpoint: CheckpointDisposition::AcknowledgedUnverifiedDurability,
        }]
    }

    fn apply_tick(&mut self, at: Duration) -> Vec<Action> {
        if self.state == PolicyState::CheckpointRequested
            && self
                .checkpoint_deadline
                .is_some_and(|deadline| at >= deadline)
        {
            self.active_request_id = None;
            self.checkpoint_deadline = None;
            self.state = PolicyState::Terminating;
            self.intervention_started = true;
            self.term_deadline = Some(at.saturating_add(self.config.term_grace));
            return vec![Action::SendTerm {
                checkpoint: CheckpointDisposition::TimedOut,
            }];
        }

        if matches!(
            self.state,
            PolicyState::Terminating | PolicyState::SupervisorError
        ) && self.term_deadline.is_some_and(|deadline| at >= deadline)
        {
            self.state = PolicyState::Emergency;
            return vec![Action::SendKill];
        }

        if !self.intervention_started
            && matches!(self.state, PolicyState::Normal | PolicyState::Warning)
            && self.config.wall_time.is_some_and(|limit| at >= limit)
        {
            return self.begin_graceful(at, 0, CheckpointDisposition::SkippedNotNegotiated);
        }
        Vec::new()
    }

    fn apply_external_signal(&mut self, at: Duration, signal: SignalNumber) -> Vec<Action> {
        if matches!(
            self.state,
            PolicyState::Terminating | PolicyState::SupervisorError | PolicyState::Emergency
        ) {
            self.state = PolicyState::Emergency;
            self.intervention_started = true;
            return vec![Action::SendKill];
        }
        self.active_request_id = None;
        self.checkpoint_deadline = None;
        self.state = PolicyState::Terminating;
        self.intervention_started = true;
        self.term_deadline = Some(at.saturating_add(self.config.term_grace));
        vec![Action::ForwardSignal(signal)]
    }

    /// Clean up owned-group survivors after the root command has already exited.
    ///
    /// Only a live, not-yet-escalating machine starts the TERM → grace → KILL sequence; an
    /// already-terminating, terminal, or non-enforcing machine treats this as a no-op so cleanup
    /// never cancels an in-flight shutdown or re-escalates it.
    fn apply_root_exited(&mut self, at: Duration) -> Vec<Action> {
        if !matches!(
            self.state,
            PolicyState::Normal | PolicyState::Warning | PolicyState::CheckpointRequested
        ) {
            return Vec::new();
        }
        self.active_request_id = None;
        self.checkpoint_deadline = None;
        self.state = PolicyState::Terminating;
        self.intervention_started = true;
        self.term_deadline = Some(at.saturating_add(self.config.term_grace));
        vec![Action::SendTerm {
            checkpoint: CheckpointDisposition::SkippedRootExited,
        }]
    }

    /// Begin a supervisor-initiated shutdown because the launching parent has exited.
    ///
    /// Only a live, not-yet-escalating machine acts. Unlike root-exit cleanup, an in-flight
    /// checkpoint request is NOT cancelled: the worker was promised its acknowledgement window,
    /// and the parent's death does not change what the workload is doing. A suppressed event
    /// produces no transition and no signal; the runtime's `parent_exited_at_ms` is the evidence.
    fn apply_parent_exited(&mut self, at: Duration) -> Vec<Action> {
        if !matches!(self.state, PolicyState::Normal | PolicyState::Warning) {
            return Vec::new();
        }
        self.active_request_id = None;
        self.checkpoint_deadline = None;
        self.state = PolicyState::Terminating;
        self.intervention_started = true;
        self.term_deadline = Some(at.saturating_add(self.config.term_grace));
        vec![Action::SendTerm {
            checkpoint: CheckpointDisposition::SkippedParentExited,
        }]
    }

    fn apply_actuation_failed(
        &mut self,
        at: Duration,
        action: ActuationKind,
        failure: ActuationFailure,
    ) -> Vec<Action> {
        match action {
            ActuationKind::Checkpoint if self.state == PolicyState::CheckpointRequested => {
                self.active_request_id = None;
                self.checkpoint_deadline = None;
                self.state = PolicyState::Terminating;
                self.intervention_started = true;
                self.term_deadline = Some(at.saturating_add(self.config.term_grace));
                vec![Action::SendTerm {
                    checkpoint: CheckpointDisposition::SkippedCheckpointFailure,
                }]
            }
            ActuationKind::Term | ActuationKind::ForwardSignal => {
                self.active_request_id = None;
                self.checkpoint_deadline = None;
                self.term_deadline = None;
                self.state = PolicyState::Emergency;
                self.intervention_started = true;
                vec![Action::SendKill]
            }
            ActuationKind::Kill | ActuationKind::Checkpoint => {
                self.active_request_id = None;
                self.checkpoint_deadline = None;
                self.term_deadline = None;
                self.state = PolicyState::SupervisorError;
                self.intervention_started = true;
                vec![Action::ReportSupervisorError { action, failure }]
            }
        }
    }

    fn apply_supervisor_fault(&mut self, at: Duration) -> Vec<Action> {
        let signal_already_requested = self.intervention_started;
        self.active_request_id = None;
        self.checkpoint_deadline = None;
        self.state = PolicyState::SupervisorError;
        match self.mode {
            PolicyMode::Observe => vec![Action::StopObserving],
            PolicyMode::Enforce if signal_already_requested => Vec::new(),
            PolicyMode::Enforce => {
                self.intervention_started = true;
                self.term_deadline = Some(at.saturating_add(self.config.term_grace));
                vec![Action::SendTerm {
                    checkpoint: CheckpointDisposition::SkippedSupervisorFailure,
                }]
            }
        }
    }

    fn fail_clock(&mut self) -> Vec<Action> {
        if self.intervention_started || self.state == PolicyState::SupervisorError {
            return Vec::new();
        }
        self.state = PolicyState::SupervisorError;
        match self.mode {
            PolicyMode::Observe => vec![Action::RecordMissing, Action::StopObserving],
            PolicyMode::Enforce => {
                self.intervention_started = true;
                self.term_deadline = self
                    .last_event_at
                    .map(|at| at.saturating_add(self.config.term_grace));
                vec![
                    Action::RecordMissing,
                    Action::SendTerm {
                        checkpoint: CheckpointDisposition::SkippedObservationFailure,
                    },
                ]
            }
        }
    }
}
