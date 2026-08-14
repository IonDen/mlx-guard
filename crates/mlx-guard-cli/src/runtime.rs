use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::thread;
use std::time::{Duration, Instant};

use mlx_guard_core::{
    Actuation, ActuationFailure, ActuationOutcome, Capabilities, CheckpointBinding,
    CheckpointChannel, CheckpointNonce, CheckpointRecord, CheckpointStatus,
    CheckpointWorkerEndpoint, ClientReady, EscapeEvidence, Event, FootprintSampler,
    IdentityTracker, InterventionEngine, JournalDurability, JournalEntry, JournalHeader,
    JournalRecord, MAX_SAMPLE_HISTORY_CAPACITY, NativeAdvisoryObserver, NativeProcessInventory,
    ObserveCalibration, Observed, OwnedProcess, PersistenceAttempt, PlatformSupport, PolicyConfig,
    PolicyMachine, PrivacyDefaults, ProcessInterventionActuator, REPORT_SCHEMA_VERSION,
    ReportConfiguration, ReportMode, ResilientJournal, RootOutcome, RunIdentity, SamplingConfig,
    SecureJournal, SignalRecord, SignalResult, SignalTarget, StdioMode, SupervisorOutcome,
    TerminalKind, TerminalOutcome, TerminalSignalMonitor, TransitionRecord, UnavailableReason,
    VERSION, checkpoint_signal_usr1, platform_support,
};

use crate::{CommandMode, CommonOptions, ObserveOptions, ParsedCli, RunOptions, policy_band_step};

const REQUIRED_BREACH_SAMPLES: u32 = 2;
const MAX_MISSING_SAMPLES: u32 = 3;
const CHECKPOINT_TIMEOUT: Duration = Duration::from_millis(100);
const TERM_GRACE: Duration = Duration::from_secs(1);

/// Complete user-visible result of one parsed command execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeResult {
    pub outcome: SupervisorOutcome,
    pub stdout: String,
    pub stderr: String,
}

impl RuntimeResult {
    fn failure(outcome: SupervisorOutcome, message: &str) -> Self {
        Self {
            outcome,
            stdout: String::new(),
            stderr: format!("mlx-guard: {message}\n"),
        }
    }
}

/// Execute one validated CLI command through the native supervisor.
#[must_use]
pub fn execute(parsed: ParsedCli) -> RuntimeResult {
    if platform_support() != PlatformSupport::Darwin {
        return RuntimeResult::failure(
            SupervisorOutcome::SupervisorFailure,
            "OS-accounted footprint observation is unsupported on this platform",
        );
    }
    match parsed.mode {
        CommandMode::Observe(options) => execute_observe(&options),
        CommandMode::Run(options) => execute_run(&options),
    }
}

fn execute_observe(options: &ObserveOptions) -> RuntimeResult {
    let ready = match ClientReady::take(options.common.client_ready_fd) {
        Ok(ready) => ready,
        Err(error) => {
            return RuntimeResult::failure(
                SupervisorOutcome::InvalidConfiguration,
                &error.to_string(),
            );
        }
    };
    let sample_config = match sampling_config(&options.common) {
        Ok(config) => config,
        Err(result) => return result,
    };
    let (inventory, journal, sequence) = match initialize_observe(&options.common) {
        Ok(values) => values,
        Err(result) => return result,
    };
    let terminal_signals = match TerminalSignalMonitor::install() {
        Ok(monitor) => monitor,
        Err(error) => {
            return finalize_without_worker(
                journal,
                sequence,
                SupervisorOutcome::SupervisorFailure,
                &error.to_string(),
            );
        }
    };

    let prepared = match prepare_observe_worker(&options.common, inventory, sample_config) {
        Ok(prepared) => prepared,
        Err(failure) => {
            return finalize_without_worker(
                journal,
                sequence,
                failure.outcome,
                &failure.diagnostic,
            );
        }
    };
    let runtime = ObserveRuntime {
        inventory,
        process: prepared.process,
        journal: ResilientJournal::new(journal),
        sequence,
        sampler: FootprintSampler::new(prepared.sample_config, prepared.tracker),
        sample_config: prepared.sample_config,
        sample_interval: options.common.sample_interval,
        advisory: NativeAdvisoryObserver::new(),
        calibration: ObserveCalibration::new(),
        report_samples: VecDeque::with_capacity(MAX_SAMPLE_HISTORY_CAPACITY),
        policy: PolicyMachine::observe(
            sample_config.max_sample_age(),
            options.common.sample_interval,
            MAX_MISSING_SAMPLES,
        ),
        observation_failed: false,
        terminal_signals,
        terminal_signal_count: 0,
        started: Instant::now(),
        root_outcome: None,
        final_footprint: Observed::Unknown,
        escape_detected: false,
    };
    ready.notify();
    runtime.run()
}

fn initialize_observe(
    common: &CommonOptions,
) -> Result<(NativeProcessInventory, SecureJournal, u64), RuntimeResult> {
    initialize(common, observe_configuration(common), false)
}

fn initialize(
    common: &CommonOptions,
    configuration: ReportConfiguration,
    checkpoint_available: bool,
) -> Result<(NativeProcessInventory, SecureJournal, u64), RuntimeResult> {
    let inventory = NativeProcessInventory::new();
    if inventory.probe_footprint().is_err() {
        return Err(RuntimeResult::failure(
            SupervisorOutcome::SupervisorFailure,
            "OS-accounted footprint observation is unavailable",
        ));
    }
    let Ok(run) = run_identity(common) else {
        return Err(RuntimeResult::failure(
            SupervisorOutcome::SupervisorFailure,
            "secure run identity generation failed",
        ));
    };
    let header = JournalHeader {
        schema_version: REPORT_SCHEMA_VERSION,
        package_version: VERSION.to_owned(),
        run,
        capabilities: Capabilities {
            darwin_footprint: Observed::Available { value: true },
            owned_process_group: Observed::Available { value: true },
            checkpoint_channel: if checkpoint_available {
                Observed::Available { value: true }
            } else {
                Observed::Unavailable {
                    reason: UnavailableReason::NotNegotiated,
                }
            },
        },
        configuration,
        privacy: PrivacyDefaults::default(),
    };
    let mut journal = SecureJournal::initialize(&common.report_path).map_err(|error| {
        RuntimeResult::failure(
            SupervisorOutcome::PartialArtifactFailure,
            &error.to_string(),
        )
    })?;
    let mut sequence = 0;
    append(
        &mut journal,
        &mut sequence,
        JournalEntry::Header(Box::new(header)),
        JournalDurability::Sync,
    )
    .map_err(|error| {
        RuntimeResult::failure(
            SupervisorOutcome::PartialArtifactFailure,
            &error.to_string(),
        )
    })?;
    Ok((inventory, journal, sequence))
}

fn execute_run(options: &RunOptions) -> RuntimeResult {
    let ready = match ClientReady::take(options.common.client_ready_fd) {
        Ok(ready) => ready,
        Err(error) => {
            return RuntimeResult::failure(
                SupervisorOutcome::InvalidConfiguration,
                &error.to_string(),
            );
        }
    };
    let (mut checkpoint_channel, checkpoint_endpoint, initial_request_id) =
        match checkpoint_channel() {
            Ok(values) => values,
            Err(result) => return result,
        };
    let policy_config = run_policy_config(options);
    let policy =
        match PolicyMachine::enforce_with_initial_request_id(policy_config, initial_request_id) {
            Ok(policy) => policy,
            Err(error) => {
                return RuntimeResult::failure(
                    SupervisorOutcome::InvalidConfiguration,
                    &error.to_string(),
                );
            }
        };
    if let Err(error) = checkpoint_channel.begin_negotiation() {
        return RuntimeResult::failure(SupervisorOutcome::SupervisorFailure, &error.to_string());
    }
    let sample_config = match sampling_config(&options.common) {
        Ok(config) => config,
        Err(result) => return result,
    };
    let (inventory, journal, sequence) =
        match initialize(&options.common, run_configuration(options), true) {
            Ok(values) => values,
            Err(result) => return result,
        };
    let terminal_signals = match TerminalSignalMonitor::install() {
        Ok(monitor) => monitor,
        Err(error) => {
            return finalize_without_worker(
                journal,
                sequence,
                SupervisorOutcome::SupervisorFailure,
                &error.to_string(),
            );
        }
    };
    let prepared = match prepare_run_worker(options, inventory, checkpoint_endpoint, sample_config)
    {
        Ok(prepared) => prepared,
        Err(failure) => {
            return finalize_without_worker(
                journal,
                sequence,
                failure.outcome,
                &failure.diagnostic,
            );
        }
    };
    let checkpoint_signal = checkpoint_signal_usr1();
    let binding = CheckpointBinding::new(
        &mut checkpoint_channel,
        prepared.checkpoint_endpoint,
        checkpoint_signal,
    );
    let actuator = ProcessInterventionActuator::new(&prepared.process, Some(binding));
    let runtime = RunRuntime {
        inventory,
        process: prepared.process,
        journal: ResilientJournal::new(journal),
        sequence,
        sampler: FootprintSampler::new(prepared.sample_config, prepared.tracker),
        sample_config: prepared.sample_config,
        sample_interval: options.common.sample_interval,
        advisory: NativeAdvisoryObserver::new(),
        calibration: ObserveCalibration::new(),
        report_samples: VecDeque::with_capacity(MAX_SAMPLE_HISTORY_CAPACITY),
        engine: InterventionEngine::new(policy, actuator),
        checkpoint_negotiation: CheckpointNegotiation::Pending,
        checkpoint_status: CheckpointStatus::NotNegotiated,
        checkpoint_at_ms: None,
        terminal_signals,
        started: Instant::now(),
        root_outcome: None,
        final_footprint: Observed::Unknown,
        escape_detected: false,
        intervention_started: false,
    };
    ready.notify();
    runtime.run()
}

struct PreparedRun {
    process: OwnedProcess,
    tracker: IdentityTracker,
    sample_config: SamplingConfig,
    checkpoint_endpoint: mlx_guard_core::CheckpointEndpoint,
}

struct PreparedObserve {
    process: OwnedProcess,
    tracker: IdentityTracker,
    sample_config: SamplingConfig,
}

struct RunLaunchFailure {
    outcome: SupervisorOutcome,
    diagnostic: String,
}

fn prepare_observe_worker(
    common: &CommonOptions,
    inventory: NativeProcessInventory,
    sample_config: SamplingConfig,
) -> Result<PreparedObserve, RunLaunchFailure> {
    let process =
        OwnedProcess::launch(&launch_options(common)).map_err(|error| RunLaunchFailure {
            outcome: launch_outcome(error.kind()),
            diagnostic: error.to_string(),
        })?;
    let root_pid = i32::try_from(process.root_pid()).map_err(|_| RunLaunchFailure {
        outcome: SupervisorOutcome::SupervisorFailure,
        diagnostic: "root process identity is invalid".to_owned(),
    })?;
    let root = inventory
        .inspect(root_pid)
        .map_err(|_| RunLaunchFailure {
            outcome: SupervisorOutcome::SupervisorFailure,
            diagnostic: "root process identity could not be established".to_owned(),
        })?
        .identity;
    let tracker =
        IdentityTracker::new(root, process.process_group_id()).map_err(|_| RunLaunchFailure {
            outcome: SupervisorOutcome::SupervisorFailure,
            diagnostic: "owned process identity could not be established".to_owned(),
        })?;
    Ok(PreparedObserve {
        process,
        tracker,
        sample_config,
    })
}

fn prepare_run_worker(
    options: &RunOptions,
    inventory: NativeProcessInventory,
    inherited_checkpoint: CheckpointWorkerEndpoint,
    sample_config: SamplingConfig,
) -> Result<PreparedRun, RunLaunchFailure> {
    let process = OwnedProcess::launch_with_checkpoint(
        &launch_options(&options.common),
        inherited_checkpoint,
    )
    .map_err(|error| RunLaunchFailure {
        outcome: launch_outcome(error.kind()),
        diagnostic: error.to_string(),
    })?;
    let root_pid = i32::try_from(process.root_pid()).map_err(|_| RunLaunchFailure {
        outcome: SupervisorOutcome::SupervisorFailure,
        diagnostic: "root process identity is invalid".to_owned(),
    })?;
    let root = inventory
        .inspect(root_pid)
        .map_err(|_| RunLaunchFailure {
            outcome: SupervisorOutcome::SupervisorFailure,
            diagnostic: "root process identity could not be established".to_owned(),
        })?
        .identity;
    let tracker =
        IdentityTracker::new(root, process.process_group_id()).map_err(|_| RunLaunchFailure {
            outcome: SupervisorOutcome::SupervisorFailure,
            diagnostic: "owned process identity could not be established".to_owned(),
        })?;
    let checkpoint_endpoint = process
        .negotiate_checkpoint_endpoint(process.root_pid())
        .map_err(|error| RunLaunchFailure {
            outcome: SupervisorOutcome::SupervisorFailure,
            diagnostic: error.to_string(),
        })?;
    Ok(PreparedRun {
        process,
        tracker,
        sample_config,
        checkpoint_endpoint,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckpointNegotiation {
    Pending,
    Ready,
    Unavailable,
}

struct RunRuntime<'a> {
    inventory: NativeProcessInventory,
    process: OwnedProcess,
    journal: ResilientJournal<SecureJournal>,
    sequence: u64,
    sampler: FootprintSampler,
    sample_config: SamplingConfig,
    sample_interval: Duration,
    advisory: NativeAdvisoryObserver,
    calibration: ObserveCalibration,
    report_samples: VecDeque<mlx_guard_core::SampleWindow>,
    engine: InterventionEngine<ProcessInterventionActuator<'a>>,
    checkpoint_negotiation: CheckpointNegotiation,
    checkpoint_status: CheckpointStatus,
    checkpoint_at_ms: Option<u64>,
    terminal_signals: TerminalSignalMonitor,
    started: Instant,
    root_outcome: Option<RootOutcome>,
    final_footprint: Observed<u64>,
    escape_detected: bool,
    intervention_started: bool,
}

impl RunRuntime<'_> {
    fn run(mut self) -> RuntimeResult {
        let completion = self.sample_until_exit();
        let at = self.started.elapsed();
        let (outcome, kind, diagnostic) = match completion {
            RunCompletion::Root(root_outcome) => {
                self.apply_event(Event::ProcessExited {
                    at,
                    final_footprint_bytes: observed_value(&self.final_footprint),
                });
                if self.intervention_started {
                    (
                        SupervisorOutcome::PolicyIntervention,
                        TerminalKind::PolicyIntervention,
                        None,
                    )
                } else {
                    (
                        supervisor_outcome(root_outcome),
                        terminal_kind(root_outcome),
                        None,
                    )
                }
            }
            RunCompletion::SupervisorFailure(diagnostic) => (
                SupervisorOutcome::SupervisorFailure,
                TerminalKind::SupervisorFailure,
                Some(diagnostic),
            ),
        };
        let terminal = TerminalOutcome {
            at_ms: duration_ms(at),
            kind,
            final_footprint_bytes: self.final_footprint.clone(),
        };
        self.record_terminal(terminal);
        let notice = self.journal.stderr_notice();
        let Some(mut journal) = self.journal.into_inner() else {
            return RuntimeResult {
                outcome: SupervisorOutcome::PartialArtifactFailure,
                stdout: String::new(),
                stderr: format!(
                    "{}\n",
                    notice.unwrap_or("mlx-guard: artifact persistence failed")
                ),
            };
        };
        match journal.finalize() {
            Ok(artifacts) => RuntimeResult {
                outcome,
                stdout: artifacts.summary,
                stderr: diagnostic
                    .map_or_else(String::new, |message| format!("mlx-guard: {message}\n")),
            },
            Err(error) => RuntimeResult::failure(
                SupervisorOutcome::PartialArtifactFailure,
                &error.to_string(),
            ),
        }
    }

    fn sample_until_exit(&mut self) -> RunCompletion {
        loop {
            if self.root_outcome.is_none() {
                self.root_outcome = match self.process.try_wait_root() {
                    Ok(outcome) => outcome,
                    Err(error) => return self.fail_supervisor(error.to_string()),
                };
            }
            let group_exists = match self.process.owned_group_exists() {
                Ok(exists) => exists,
                Err(error) => return self.fail_supervisor(error.to_string()),
            };
            if let Some(outcome) = self.root_outcome
                && !group_exists
            {
                return RunCompletion::Root(outcome);
            }

            self.poll_checkpoint_channel();
            let terminal_signals = match self.terminal_signals.poll() {
                Ok(signals) => signals,
                Err(error) => return self.fail_supervisor(error.to_string()),
            };
            for signal in terminal_signals {
                self.apply_event(Event::ExternalSignal {
                    at: self.started.elapsed(),
                    signal,
                });
            }

            self.sample_once();
            let now = self.started.elapsed();
            self.apply_event(Event::Tick { at: now });
            let sample_delay = self
                .sampler
                .delay_until_next(now)
                .unwrap_or(self.sample_interval);
            let policy_delay = self
                .engine
                .policy()
                .next_deadline()
                .map_or(sample_delay, |deadline| deadline.saturating_sub(now));
            let delay = sample_delay.min(policy_delay);
            if !delay.is_zero() {
                thread::sleep(delay);
            }
        }
    }

    fn fail_supervisor(&mut self, diagnostic: String) -> RunCompletion {
        self.apply_event(Event::SupervisorFault {
            at: self.started.elapsed(),
        });
        if let Some(deadline) = self.engine.policy().next_deadline() {
            loop {
                let now = self.started.elapsed();
                if now >= deadline {
                    self.apply_event(Event::Tick { at: now });
                    break;
                }
                thread::sleep(deadline.saturating_sub(now));
            }
        }
        RunCompletion::SupervisorFailure(diagnostic)
    }

    fn poll_checkpoint_channel(&mut self) {
        let now = self.started.elapsed();
        match self.checkpoint_negotiation {
            CheckpointNegotiation::Pending => {
                self.checkpoint_negotiation =
                    match self.engine.actuator_mut().poll_checkpoint_ready() {
                        Ok(true) => CheckpointNegotiation::Ready,
                        Ok(false) => return,
                        Err(_) => CheckpointNegotiation::Unavailable,
                    };
            }
            CheckpointNegotiation::Ready => {}
            CheckpointNegotiation::Unavailable => return,
        }
        if self.checkpoint_negotiation == CheckpointNegotiation::Ready
            && self.engine.policy().state() == mlx_guard_core::PolicyState::CheckpointRequested
        {
            match self.engine.actuator_mut().poll_checkpoint(now) {
                Ok(observation) => {
                    if let Some(event) = observation.event {
                        self.apply_event(event);
                    }
                }
                Err(failure) => self.apply_event(Event::ActuationFailed {
                    at: now,
                    action: mlx_guard_core::ActuationKind::Checkpoint,
                    failure,
                }),
            }
        }
    }

    fn sample_once(&mut self) {
        let sample = self
            .sampler
            .sample_native(&self.inventory, self.started)
            .clone();
        let processed_at = self.started.elapsed();
        let policy_event = self.sampler.policy_event(&sample, processed_at);
        let advisory = self
            .advisory
            .snapshot(processed_at, self.sample_config.max_sample_age());
        let window = self
            .calibration
            .record_sample(&sample, processed_at, &advisory);
        retain_recent_sample(&mut self.report_samples, window.clone());
        self.final_footprint = window.aggregate_footprint_bytes.clone();
        self.escape_detected |= !sample.escaped_identities.is_empty();
        if sample.sequence < MAX_SAMPLE_HISTORY_CAPACITY as u64 {
            self.record(
                JournalEntry::Sample(Box::new(window.clone())),
                JournalDurability::Buffered,
            );
        }
        self.apply_event(Event::Sample(policy_event));
    }

    fn apply_event(&mut self, event: Event) {
        let external_signal = matches!(event, Event::ExternalSignal { .. });
        if let Event::CheckpointAck {
            at,
            authenticated: true,
            ..
        } = &event
        {
            self.checkpoint_status = CheckpointStatus::AcknowledgedUnverifiedDurability;
            self.checkpoint_at_ms = Some(duration_ms(*at));
        }
        let previous_attempts = self.engine.evidence().total_attempts;
        let final_footprint = observed_value(&self.final_footprint);
        let Self {
            engine,
            journal,
            sequence,
            ..
        } = self;
        let _ = engine.handle_with_transition_observer(event, |at, from, to| {
            record_resilient(
                journal,
                sequence,
                JournalEntry::Transition(TransitionRecord {
                    at_ms: duration_ms(at),
                    from,
                    to,
                    aggregate_footprint_bytes: final_footprint,
                }),
                JournalDurability::Sync,
            );
        });
        let new_attempts = engine
            .evidence()
            .total_attempts
            .saturating_sub(previous_attempts);
        self.intervention_started |= !external_signal && new_attempts > 0;
        let records = engine.evidence().records();
        let new_record_count = usize::try_from(new_attempts).unwrap_or(usize::MAX);
        let start = records.len().saturating_sub(new_record_count);
        let new_records = records.iter().skip(start).copied().collect::<Vec<_>>();
        let signals = new_records
            .iter()
            .filter_map(|record| {
                signal_record(
                    record.requested_at,
                    record.action,
                    record.result,
                    Some(checkpoint_signal_usr1().get()),
                )
            })
            .collect::<Vec<_>>();
        for record in new_records {
            match (record.action, record.result) {
                (Actuation::Checkpoint { .. }, Ok(ActuationOutcome::Delivered)) => {
                    self.checkpoint_status = CheckpointStatus::RequestedUnverified;
                    self.checkpoint_at_ms = Some(duration_ms(record.requested_at));
                }
                (
                    Actuation::Term {
                        checkpoint: mlx_guard_core::CheckpointDisposition::TimedOut,
                    },
                    _,
                ) => {
                    self.checkpoint_status = CheckpointStatus::TimedOut;
                    self.checkpoint_at_ms = Some(duration_ms(record.requested_at));
                }
                (Actuation::Checkpoint { .. }, Err(_))
                    if self.checkpoint_negotiation == CheckpointNegotiation::Ready =>
                {
                    self.checkpoint_status = CheckpointStatus::Cancelled;
                    self.checkpoint_at_ms = Some(duration_ms(record.requested_at));
                }
                _ => {}
            }
        }
        for signal in signals {
            record_resilient(
                journal,
                sequence,
                JournalEntry::Signal(signal),
                JournalDurability::Sync,
            );
        }
    }

    fn record_terminal(&mut self, outcome: TerminalOutcome) {
        self.record_recent_sample_history();
        self.record(
            JournalEntry::Checkpoint(CheckpointRecord {
                status: self.checkpoint_status,
                at_ms: self.checkpoint_at_ms,
            }),
            JournalDurability::Sync,
        );
        self.record(
            JournalEntry::Escape(EscapeEvidence {
                detected: Observed::Available {
                    value: self.escape_detected,
                },
            }),
            JournalDurability::Buffered,
        );
        self.record(JournalEntry::Outcome(outcome), JournalDurability::Sync);
    }

    fn record_recent_sample_history(&mut self) {
        if self
            .sampler
            .history()
            .next()
            .is_none_or(|sample| sample.sequence == 0)
        {
            return;
        }
        self.record(JournalEntry::SampleHistoryReset, JournalDurability::Sync);
        let samples = self.report_samples.iter().cloned().collect::<Vec<_>>();
        for sample in samples {
            self.record(
                JournalEntry::Sample(Box::new(sample)),
                JournalDurability::Buffered,
            );
        }
    }

    fn record(&mut self, entry: JournalEntry, durability: JournalDurability) {
        record_resilient(&mut self.journal, &mut self.sequence, entry, durability);
    }
}

#[derive(Debug, Eq, PartialEq)]
enum RunCompletion {
    Root(RootOutcome),
    SupervisorFailure(String),
}

struct ObserveRuntime {
    inventory: NativeProcessInventory,
    process: OwnedProcess,
    journal: ResilientJournal<SecureJournal>,
    sequence: u64,
    sampler: FootprintSampler,
    sample_config: SamplingConfig,
    sample_interval: Duration,
    advisory: NativeAdvisoryObserver,
    calibration: ObserveCalibration,
    report_samples: VecDeque<mlx_guard_core::SampleWindow>,
    policy: PolicyMachine,
    observation_failed: bool,
    terminal_signals: TerminalSignalMonitor,
    terminal_signal_count: u32,
    started: Instant,
    root_outcome: Option<RootOutcome>,
    final_footprint: Observed<u64>,
    escape_detected: bool,
}

impl ObserveRuntime {
    fn run(mut self) -> RuntimeResult {
        let completion = self.sample_until_exit();
        let (outcome, kind, diagnostic, relinquish) = match completion {
            ObserveCompletion::Root(root_outcome) => (
                supervisor_outcome(root_outcome),
                terminal_kind(root_outcome),
                None,
                false,
            ),
            ObserveCompletion::ObservationFailure => (
                SupervisorOutcome::SupervisorFailure,
                TerminalKind::SupervisorFailure,
                Some("footprint observation failed".to_owned()),
                true,
            ),
            ObserveCompletion::SupervisorFailure(diagnostic) => (
                SupervisorOutcome::SupervisorFailure,
                TerminalKind::SupervisorFailure,
                Some(diagnostic),
                true,
            ),
        };
        let terminal = TerminalOutcome {
            at_ms: duration_ms(self.started.elapsed()),
            kind,
            final_footprint_bytes: self.final_footprint.clone(),
        };
        self.record_terminal(terminal);
        if relinquish {
            self.process.relinquish();
        }
        let notice = self.journal.stderr_notice();
        let Some(mut journal) = self.journal.into_inner() else {
            return RuntimeResult {
                outcome: SupervisorOutcome::PartialArtifactFailure,
                stdout: String::new(),
                stderr: format!(
                    "{}\n",
                    notice.unwrap_or("mlx-guard: artifact persistence failed")
                ),
            };
        };
        match journal.finalize() {
            Ok(artifacts) => RuntimeResult {
                outcome,
                stdout: artifacts.summary,
                stderr: diagnostic
                    .map_or_else(String::new, |message| format!("mlx-guard: {message}\n")),
            },
            Err(error) => RuntimeResult::failure(
                SupervisorOutcome::PartialArtifactFailure,
                &error.to_string(),
            ),
        }
    }

    fn sample_until_exit(&mut self) -> ObserveCompletion {
        loop {
            if self.root_outcome.is_none() {
                self.root_outcome = match self.process.try_wait_root() {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        return ObserveCompletion::SupervisorFailure(error.to_string());
                    }
                };
            }
            let group_exists = match self.process.owned_group_exists() {
                Ok(exists) => exists,
                Err(error) => {
                    return ObserveCompletion::SupervisorFailure(error.to_string());
                }
            };
            if let Some(outcome) = self.root_outcome
                && !group_exists
            {
                return ObserveCompletion::Root(outcome);
            }
            if let Err(diagnostic) = self.forward_terminal_signals() {
                return ObserveCompletion::SupervisorFailure(diagnostic);
            }
            if !self.observation_failed {
                self.sample_once();
            }
            if self.observation_failed {
                return ObserveCompletion::ObservationFailure;
            }
            let delay = self
                .sampler
                .delay_until_next(self.started.elapsed())
                .unwrap_or(self.sample_interval);
            if !delay.is_zero() {
                thread::sleep(delay);
            }
        }
    }

    fn forward_terminal_signals(&mut self) -> Result<(), String> {
        let signals = self
            .terminal_signals
            .poll()
            .map_err(|error| error.to_string())?;
        for signal in signals {
            let (delivered_signal, result) = if self.terminal_signal_count == 0 {
                (signal.get(), self.process.forward_terminal_signal(signal))
            } else {
                (9, self.process.kill_group())
            };
            self.terminal_signal_count = self.terminal_signal_count.saturating_add(1);
            let result = result.map_err(|error| error.to_string())?;
            self.record(
                JournalEntry::Signal(SignalRecord {
                    at_ms: duration_ms(self.started.elapsed()),
                    signal: delivered_signal,
                    target: SignalTarget::OwnedProcessGroup,
                    result,
                }),
                JournalDurability::Sync,
            );
        }
        Ok(())
    }

    fn sample_once(&mut self) {
        let sample = self
            .sampler
            .sample_native(&self.inventory, self.started)
            .clone();
        let processed_at = self.started.elapsed();
        let policy_event = self.sampler.policy_event(&sample, processed_at);
        let advisory = self
            .advisory
            .snapshot(processed_at, self.sample_config.max_sample_age());
        let window = self
            .calibration
            .record_sample(&sample, processed_at, &advisory);
        retain_recent_sample(&mut self.report_samples, window.clone());
        self.final_footprint = window.aggregate_footprint_bytes.clone();
        self.escape_detected |= !sample.escaped_identities.is_empty();
        if sample.sequence < MAX_SAMPLE_HISTORY_CAPACITY as u64 {
            self.record(
                JournalEntry::Sample(Box::new(window.clone())),
                JournalDurability::Buffered,
            );
        }
        let previous_state = self.policy.state();
        let _ = self.policy.apply(Event::Sample(policy_event));
        let current_state = self.policy.state();
        if previous_state != current_state {
            self.record(
                JournalEntry::Transition(TransitionRecord {
                    at_ms: duration_ms(processed_at),
                    from: previous_state,
                    to: current_state,
                    aggregate_footprint_bytes: observed_value(&window.aggregate_footprint_bytes),
                }),
                JournalDurability::Sync,
            );
        }
        self.observation_failed = current_state == mlx_guard_core::PolicyState::SupervisorError;
    }

    fn record_terminal(&mut self, outcome: TerminalOutcome) {
        self.record_recent_sample_history();
        self.record(
            JournalEntry::Checkpoint(CheckpointRecord {
                status: CheckpointStatus::NotNegotiated,
                at_ms: None,
            }),
            JournalDurability::Sync,
        );
        self.record(
            JournalEntry::Escape(EscapeEvidence {
                detected: Observed::Available {
                    value: self.escape_detected,
                },
            }),
            JournalDurability::Buffered,
        );
        self.record(JournalEntry::Outcome(outcome), JournalDurability::Sync);
    }

    fn record_recent_sample_history(&mut self) {
        if self
            .sampler
            .history()
            .next()
            .is_none_or(|sample| sample.sequence == 0)
        {
            return;
        }
        self.record(JournalEntry::SampleHistoryReset, JournalDurability::Sync);
        let samples = self.report_samples.iter().cloned().collect::<Vec<_>>();
        for sample in samples {
            self.record(
                JournalEntry::Sample(Box::new(sample)),
                JournalDurability::Buffered,
            );
        }
    }

    fn record(&mut self, entry: JournalEntry, durability: JournalDurability) {
        let record = JournalRecord::new(self.sequence, entry);
        if self.journal.record(&record, durability) == PersistenceAttempt::Persisted {
            self.sequence = self.sequence.saturating_add(1);
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ObserveCompletion {
    Root(RootOutcome),
    ObservationFailure,
    SupervisorFailure(String),
}

fn supervisor_outcome(outcome: RootOutcome) -> SupervisorOutcome {
    match outcome {
        RootOutcome::Exited(code) => SupervisorOutcome::ChildExited(code),
        RootOutcome::Signaled(signal) => SupervisorOutcome::ChildSignaled(signal),
    }
}

fn terminal_kind(outcome: RootOutcome) -> TerminalKind {
    match outcome {
        RootOutcome::Exited(code) => TerminalKind::ChildExited { code },
        RootOutcome::Signaled(signal) => TerminalKind::ChildSignaled {
            signal: signal.get(),
        },
    }
}

fn finalize_without_worker(
    mut journal: SecureJournal,
    mut sequence: u64,
    outcome: SupervisorOutcome,
    diagnostic: &str,
) -> RuntimeResult {
    let kind = match outcome {
        SupervisorOutcome::LaunchNotFound => TerminalKind::LaunchNotFound,
        SupervisorOutcome::LaunchNotExecutable => TerminalKind::LaunchNotExecutable,
        SupervisorOutcome::InvalidConfiguration => TerminalKind::InvalidConfiguration,
        _ => TerminalKind::SupervisorFailure,
    };
    let terminal = TerminalOutcome {
        at_ms: 0,
        kind,
        final_footprint_bytes: Observed::Unknown,
    };
    if append_terminal(&mut journal, &mut sequence, false, terminal).is_err()
        || journal.finalize().is_err()
    {
        return RuntimeResult::failure(
            SupervisorOutcome::PartialArtifactFailure,
            "launch failed and its report could not be finalized",
        );
    }
    RuntimeResult::failure(outcome, diagnostic)
}

fn launch_outcome(kind: mlx_guard_core::LaunchErrorKind) -> SupervisorOutcome {
    match kind {
        mlx_guard_core::LaunchErrorKind::NotFound => SupervisorOutcome::LaunchNotFound,
        mlx_guard_core::LaunchErrorKind::NotExecutable => SupervisorOutcome::LaunchNotExecutable,
        mlx_guard_core::LaunchErrorKind::EmptyCommand
        | mlx_guard_core::LaunchErrorKind::InvalidWorkingDirectory
        | mlx_guard_core::LaunchErrorKind::InteractiveTerminalUnsupported
        | mlx_guard_core::LaunchErrorKind::InvalidCheckpointChannel => {
            SupervisorOutcome::InvalidConfiguration
        }
        mlx_guard_core::LaunchErrorKind::SpawnFailed
        | mlx_guard_core::LaunchErrorKind::ProcessGroupValidationFailed => {
            SupervisorOutcome::SupervisorFailure
        }
    }
}

fn append_terminal(
    journal: &mut SecureJournal,
    sequence: &mut u64,
    escape_detected: bool,
    outcome: TerminalOutcome,
) -> Result<(), mlx_guard_core::StorageError> {
    append(
        journal,
        sequence,
        JournalEntry::Checkpoint(CheckpointRecord {
            status: CheckpointStatus::NotNegotiated,
            at_ms: None,
        }),
        JournalDurability::Sync,
    )?;
    append(
        journal,
        sequence,
        JournalEntry::Escape(EscapeEvidence {
            detected: Observed::Available {
                value: escape_detected,
            },
        }),
        JournalDurability::Buffered,
    )?;
    append(
        journal,
        sequence,
        JournalEntry::Outcome(outcome),
        JournalDurability::Sync,
    )
}

fn append(
    journal: &mut SecureJournal,
    sequence: &mut u64,
    entry: JournalEntry,
    durability: JournalDurability,
) -> Result<(), mlx_guard_core::StorageError> {
    journal.append(&JournalRecord::new(*sequence, entry), durability)?;
    *sequence = sequence.saturating_add(1);
    Ok(())
}

fn launch_options(common: &CommonOptions) -> mlx_guard_core::LaunchOptions {
    mlx_guard_core::LaunchOptions {
        command: common.command.clone(),
        cwd: common.cwd.clone(),
        clear_env: common.clear_env,
        env: common.env.clone(),
        stdin: StdioMode::Inherit,
        stdout: StdioMode::Inherit,
        stderr: StdioMode::Inherit,
    }
}

fn observe_configuration(common: &CommonOptions) -> ReportConfiguration {
    ReportConfiguration {
        mode: ReportMode::Observe,
        max_footprint_bytes: None,
        warning_footprint_bytes: None,
        recovery_footprint_bytes: None,
        emergency_footprint_bytes: None,
        required_breach_samples: REQUIRED_BREACH_SAMPLES,
        max_missing_samples: MAX_MISSING_SAMPLES,
        wall_time_ms: None,
        sample_interval_ms: duration_ms(common.sample_interval),
        max_sample_age_ms: duration_ms(common.sample_interval.saturating_mul(2)),
        max_sample_window_ms: duration_ms(common.sample_interval),
        checkpoint_timeout_ms: None,
        term_grace_ms: duration_ms(TERM_GRACE),
    }
}

fn run_configuration(options: &RunOptions) -> ReportConfiguration {
    let policy = run_policy_config(options);
    ReportConfiguration {
        mode: ReportMode::Enforce,
        max_footprint_bytes: Some(policy.limit_bytes),
        warning_footprint_bytes: Some(policy.warning_bytes),
        recovery_footprint_bytes: Some(policy.recovery_bytes),
        emergency_footprint_bytes: Some(policy.emergency_bytes),
        required_breach_samples: policy.required_breach_samples,
        max_missing_samples: policy.max_missing_samples,
        wall_time_ms: policy.wall_time.map(duration_ms),
        sample_interval_ms: duration_ms(options.common.sample_interval),
        max_sample_age_ms: duration_ms(policy.max_sample_age),
        max_sample_window_ms: duration_ms(policy.max_sample_window),
        checkpoint_timeout_ms: policy.checkpoint_timeout.map(duration_ms),
        term_grace_ms: duration_ms(policy.term_grace),
    }
}

fn run_policy_config(options: &RunOptions) -> PolicyConfig {
    let limit = options.max_footprint_bytes;
    let threshold_step = policy_band_step(limit);
    PolicyConfig {
        limit_bytes: limit,
        warning_bytes: limit.saturating_sub(threshold_step),
        recovery_bytes: limit
            .saturating_sub(threshold_step)
            .saturating_sub(threshold_step),
        emergency_bytes: limit.saturating_add(threshold_step),
        required_breach_samples: REQUIRED_BREACH_SAMPLES,
        max_missing_samples: MAX_MISSING_SAMPLES,
        max_sample_age: options.common.sample_interval.saturating_mul(2),
        max_sample_window: options.common.sample_interval,
        checkpoint_timeout: Some(CHECKPOINT_TIMEOUT),
        term_grace: TERM_GRACE,
        wall_time: options.wall_time,
    }
}

fn sampling_config(common: &CommonOptions) -> Result<SamplingConfig, RuntimeResult> {
    SamplingConfig::new(
        common.sample_interval,
        common.sample_interval.saturating_mul(2),
        common.sample_interval,
        MAX_SAMPLE_HISTORY_CAPACITY,
    )
    .map_err(|error| {
        RuntimeResult::failure(SupervisorOutcome::InvalidConfiguration, &error.to_string())
    })
}

fn observed_value(value: &Observed<u64>) -> Option<u64> {
    match value {
        Observed::Available { value } => Some(*value),
        Observed::Unknown
        | Observed::Unavailable { .. }
        | Observed::Stale { .. }
        | Observed::Error { .. } => None,
    }
}

fn signal_record(
    at: Duration,
    action: Actuation,
    result: Result<ActuationOutcome, ActuationFailure>,
    checkpoint_signal: Option<u8>,
) -> Option<SignalRecord> {
    let (signal, target) = match action {
        Actuation::Checkpoint { .. } => {
            if matches!(
                result,
                Err(ActuationFailure::CheckpointUnavailable | ActuationFailure::CheckpointRejected)
            ) {
                return None;
            }
            (checkpoint_signal?, SignalTarget::CooperativeEndpoint)
        }
        Actuation::Term { .. } => (15, SignalTarget::OwnedProcessGroup),
        Actuation::Kill => (9, SignalTarget::OwnedProcessGroup),
        Actuation::ForwardSignal(signal) => (signal.get(), SignalTarget::OwnedProcessGroup),
    };
    let result = match result {
        Ok(ActuationOutcome::Delivered) => SignalResult::Delivered,
        Ok(ActuationOutcome::ProcessMissing) | Err(ActuationFailure::ProcessMissing) => {
            SignalResult::ProcessMissing
        }
        Err(ActuationFailure::PermissionDenied) => SignalResult::PermissionDenied,
        Err(
            ActuationFailure::CheckpointUnavailable
            | ActuationFailure::CheckpointRejected
            | ActuationFailure::InvalidTarget
            | ActuationFailure::SignalFailed,
        ) => SignalResult::Failed,
    };
    Some(SignalRecord {
        at_ms: duration_ms(at),
        signal,
        target,
        result,
    })
}

fn record_resilient(
    journal: &mut ResilientJournal<SecureJournal>,
    sequence: &mut u64,
    entry: JournalEntry,
    durability: JournalDurability,
) {
    let record = JournalRecord::new(*sequence, entry);
    if journal.record(&record, durability) == PersistenceAttempt::Persisted {
        *sequence = sequence.saturating_add(1);
    }
}

fn checkpoint_channel() -> Result<(CheckpointChannel, CheckpointWorkerEndpoint, u64), RuntimeResult>
{
    let mut random = File::open("/dev/urandom").map_err(|_| {
        RuntimeResult::failure(
            SupervisorOutcome::SupervisorFailure,
            "secure checkpoint nonce generation failed",
        )
    })?;
    let mut nonce_bytes = [0_u8; 32];
    random.read_exact(&mut nonce_bytes).map_err(|_| {
        RuntimeResult::failure(
            SupervisorOutcome::SupervisorFailure,
            "secure checkpoint nonce generation failed",
        )
    })?;
    let initial_request_id = loop {
        let mut request_id_bytes = [0_u8; 8];
        random.read_exact(&mut request_id_bytes).map_err(|_| {
            RuntimeResult::failure(
                SupervisorOutcome::SupervisorFailure,
                "secure checkpoint request-id generation failed",
            )
        })?;
        let request_id = u64::from_be_bytes(request_id_bytes);
        if request_id != 0 {
            break request_id;
        }
    };
    let nonce = CheckpointNonce::from_bytes(nonce_bytes);
    CheckpointChannel::pair(nonce)
        .map(|(channel, endpoint)| (channel, endpoint, initial_request_id))
        .map_err(|error| {
            RuntimeResult::failure(SupervisorOutcome::SupervisorFailure, &error.to_string())
        })
}

fn retain_recent_sample(
    samples: &mut VecDeque<mlx_guard_core::SampleWindow>,
    sample: mlx_guard_core::SampleWindow,
) {
    if samples.len() == MAX_SAMPLE_HISTORY_CAPACITY {
        samples.pop_front();
    }
    samples.push_back(sample);
}

fn run_identity(common: &CommonOptions) -> Result<RunIdentity, ()> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| ())?;
    let mut run_id = String::with_capacity(36);
    run_id.push_str("run_");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut run_id, "{byte:02x}").map_err(|_| ())?;
    }
    RunIdentity::from_argv(&run_id, &common.command, None).map_err(|_| ())
}

fn duration_ms(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}
