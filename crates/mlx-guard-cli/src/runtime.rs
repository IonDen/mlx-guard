use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::thread;
use std::time::{Duration, Instant};

use mlx_guard_core::{
    Actuation, ActuationFailure, ActuationOutcome, Capabilities, CheckpointBinding,
    CheckpointChannel, CheckpointDisposition, CheckpointNonce, CheckpointRecord, CheckpointStatus,
    CheckpointWorkerEndpoint, ChildStatus, ClientReady, EscapeEvidence, Event, FootprintSampler,
    IdentityTracker, InterventionEngine, InterventionRecord, JournalDurability, JournalEntry,
    JournalHeader, JournalRecord, MAX_SAMPLE_HISTORY_CAPACITY, NativeAdvisoryObserver,
    NativeProcessInventory, ObserveCalibration, Observed, OnParentExit, OwnedProcess, ParentWatch,
    PersistenceAttempt, PlatformSupport, PolicyConfig, PolicyMachine, PolicyState, PrivacyDefaults,
    ProcessIdentity, ProcessInterventionActuator, REPORT_SCHEMA_VERSION, ReportConfiguration,
    ReportMode, ResilientJournal, RootOutcome, RunIdentity, SamplingConfig, SecureJournal,
    SignalReason, SignalRecord, SignalResult, SignalTarget, StdioMode, SupervisorOutcome,
    TerminalKind, TerminalOutcome, TerminalSignalMonitor, TransitionRecord, UnavailableReason,
    VERSION, checkpoint_signal_usr1, hangup_is_ignored, platform_support,
};

use crate::completion::{
    Completion, CompletionInputs, complete, supervisor_outcome, terminal_kind,
};
use crate::{
    CommandMode, CommonOptions, ObserveOptions, OnParentExitOption, ParsedCli, RunOptions,
    policy_band_step,
};

const REQUIRED_BREACH_SAMPLES: u32 = 2;
const MAX_MISSING_SAMPLES: u32 = 3;
// A real cooperative worker on a loaded machine missed the former 100 ms acknowledgement window
// (observed on the 3 vCPU CI runner); interventions happen under exactly that kind of pressure,
// so the default matches TERM_GRACE's order of magnitude. `--checkpoint-timeout` overrides it.
const DEFAULT_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(1);
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
    // The journal header carries the parent watch, so the watch is decided before the header is
    // written and never revised afterwards.
    let parent_watch = parent_watch_state(options.common.on_parent_exit);
    let (inventory, journal, sequence) = match initialize_observe(&options.common, &parent_watch) {
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
        parent_watch,
        parent_shutdown_deadline: None,
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
    parent_watch: &ParentWatchState,
) -> Result<(NativeProcessInventory, SecureJournal, u64), RuntimeResult> {
    initialize(common, observe_configuration(common, parent_watch), false)
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
    // The journal header carries the parent watch, so the watch is decided before the header is
    // written and never revised afterwards.
    let parent_watch = parent_watch_state(options.common.on_parent_exit);
    let (inventory, journal, sequence) = match initialize(
        &options.common,
        run_configuration(options, &parent_watch),
        true,
    ) {
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
        parent_watch,
        started: Instant::now(),
        root_outcome: None,
        final_footprint: Observed::Unknown,
        escape_detected: false,
        intervention_started: false,
        root_exit_applied: false,
        supervisor_error: None,
        intervention_cause: None,
        shutdown_reason: None,
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

/// Launch-time parent identity and the decision whether to watch it.
///
/// `parent` is `Some` exactly for the two watching states (`Active` enforces, `Detach` collects
/// evidence only); the three never-checked states carry `None`.
struct ParentWatchState {
    parent: Option<ProcessIdentity>,
    watch: ParentWatch,
    on_parent_exit: OnParentExit,
    exited_at_ms: Option<u64>,
}

impl ParentWatchState {
    /// A watch that is never checked, because there is no parent identity worth binding.
    const fn unwatched(watch: ParentWatch, on_parent_exit: OnParentExit) -> Self {
        Self {
            parent: None,
            watch,
            on_parent_exit,
            exited_at_ms: None,
        }
    }
}

/// Decide one run's parent watch, querying the SIGHUP disposition the decision depends on.
///
/// The disposition is queried, never installed: a `nohup`-style launcher that already disposed
/// SIGHUP to `SIG_IGN` asked, by that convention, for the run to outlive it. A failed query
/// establishes no watch at all, and the same failing system call ends this run moments later when
/// the terminal-signal monitor is installed.
fn parent_watch_state(option: OnParentExitOption) -> ParentWatchState {
    match hangup_is_ignored() {
        Ok(hangup_ignored) => {
            establish_parent_watch(NativeProcessInventory::new(), hangup_ignored, option)
        }
        Err(_) => ParentWatchState::unwatched(ParentWatch::ParentUnobservable, option.into()),
    }
}

/// Bind the launching parent's exact identity, or record why no watch was established.
fn establish_parent_watch(
    inventory: NativeProcessInventory,
    hangup_ignored: bool,
    option: OnParentExitOption,
) -> ParentWatchState {
    let on_parent_exit = OnParentExit::from(option);
    let ppid = i32::try_from(std::os::unix::process::parent_id()).unwrap_or(0);
    // Detach collects evidence and enforces nothing, so neither an already-orphaned run nor a
    // `nohup` intent is a reason to skip its watch: both only rule out enforcement.
    let wants_detach = matches!(option, OnParentExitOption::Detach);
    if ppid <= 1 && !wants_detach {
        return ParentWatchState::unwatched(ParentWatch::ParentIsLaunchd, on_parent_exit);
    }
    if hangup_ignored && !wants_detach {
        return ParentWatchState::unwatched(ParentWatch::HangupIgnored, on_parent_exit);
    }
    match inventory.inspect(ppid) {
        Ok(observation) if !observation.exited => ParentWatchState {
            parent: Some(observation.identity),
            watch: if wants_detach {
                ParentWatch::Detach
            } else {
                ParentWatch::Active
            },
            on_parent_exit,
            exited_at_ms: None,
        },
        // Typed honesty: a watch that could not be established is never reported as active.
        _ => ParentWatchState::unwatched(ParentWatch::ParentUnobservable, on_parent_exit),
    }
}

/// Report the first time the recorded parent is confirmed gone; never reports it twice.
///
/// The caller passes the one clock read it also gives the policy event, so the recorded orphan
/// time and the event that acts on it never disagree.
fn parent_has_exited(
    state: &mut ParentWatchState,
    inventory: NativeProcessInventory,
    now: Duration,
) -> bool {
    if state.exited_at_ms.is_some() {
        return false;
    }
    let Some(parent) = state.parent else {
        return false;
    };
    // On macOS a process reparents only once its real parent has exited (there is no subreaper
    // that could reparent it earlier), so a `getppid()` match against the recorded parent pid is
    // conclusive proof the parent is still alive: return immediately, with no inspection at all.
    // Confirmation only ever runs after that pre-filter fires (the pid changed). Even then, a
    // still-alive recorded identity — or any inspection hiccup that merely fails to prove it — is
    // treated as inconclusive rather than fatal: only a confirmed-gone identity latches the exit.
    if i32::try_from(std::os::unix::process::parent_id()).unwrap_or(0) == parent.pid {
        return false;
    }
    if inventory
        .inspect_expected(parent)
        .is_ok_and(|observation| !observation.exited)
    {
        // The recorded identity is still observably alive despite the pid change: retry on the
        // next tick instead of latching a false exit off one inconclusive read.
        return false;
    }
    state.exited_at_ms = Some(duration_ms(now));
    true
}

fn prepare_observe_worker(
    common: &CommonOptions,
    inventory: NativeProcessInventory,
    sample_config: SamplingConfig,
) -> Result<PreparedObserve, RunLaunchFailure> {
    let mut process =
        OwnedProcess::launch(&launch_options(common)).map_err(|error| RunLaunchFailure {
            outcome: launch_outcome(error.kind()),
            diagnostic: error.to_string(),
        })?;
    let root_pid = i32::try_from(process.root_pid()).map_err(|_| RunLaunchFailure {
        outcome: SupervisorOutcome::SupervisorFailure,
        diagnostic: "root process identity is invalid".to_owned(),
    })?;
    let root = if let Ok(observation) = inventory.inspect(root_pid) {
        observation.identity
    } else {
        if let Ok(Some(outcome)) = process.try_wait_root() {
            return Err(RunLaunchFailure {
                outcome: supervisor_outcome(outcome),
                diagnostic: "child exited before identity inspection".to_owned(),
            });
        }
        return Err(RunLaunchFailure {
            outcome: SupervisorOutcome::SupervisorFailure,
            diagnostic: "root process identity could not be established".to_owned(),
        });
    };
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
    let mut process = OwnedProcess::launch_with_checkpoint(
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
    let root = if let Ok(observation) = inventory.inspect(root_pid) {
        observation.identity
    } else {
        if let Ok(Some(outcome)) = process.try_wait_root() {
            return Err(RunLaunchFailure {
                outcome: supervisor_outcome(outcome),
                diagnostic: "child exited before identity inspection".to_owned(),
            });
        }
        return Err(RunLaunchFailure {
            outcome: SupervisorOutcome::SupervisorFailure,
            diagnostic: "root process identity could not be established".to_owned(),
        });
    };
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
    /// The launching parent's identity, the watch decision, and the orphan time once observed.
    parent_watch: ParentWatchState,
    started: Instant,
    root_outcome: Option<RootOutcome>,
    final_footprint: Observed<u64>,
    escape_detected: bool,
    /// Whether a counted intervention attempt has been made against the owned group.
    intervention_started: bool,
    /// Whether owned-group survivor cleanup has already been requested for this run.
    root_exit_applied: bool,
    /// Diagnostic latched the first time the policy machine failed closed.
    supervisor_error: Option<&'static str>,
    /// Whether the footprint limit or the wall limit opened the current intervention.
    intervention_cause: Option<SignalReason>,
    /// Why the shutdown sequence in flight was started.
    shutdown_reason: Option<SignalReason>,
}

impl RunRuntime<'_> {
    fn run(mut self) -> RuntimeResult {
        let completion = self.sample_until_exit();
        let at = self.started.elapsed();
        let supervisor_failure = match completion {
            RunCompletion::Root(_) => {
                self.apply_event(Event::ProcessExited {
                    at,
                    final_footprint_bytes: observed_value(&self.final_footprint),
                });
                None
            }
            RunCompletion::SupervisorFailure(diagnostic) => Some(diagnostic),
        };
        let Completion {
            outcome,
            kind,
            child_status,
            diagnostic,
        } = complete(CompletionInputs {
            root_outcome: self.root_outcome,
            intervention_started: self.intervention_started,
            supervisor_error: self.supervisor_error,
            supervisor_failure,
        });
        let terminal = TerminalOutcome {
            at_ms: duration_ms(at),
            kind,
            final_footprint_bytes: self.final_footprint.clone(),
            child_status,
            owned_group_survivors: None,
            parent_exited_at_ms: self.parent_watch.exited_at_ms,
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
            // Deliberately after the completion check above: a run whose work has already finished
            // must not manufacture a termination for a parent that died alongside it.
            let now = self.started.elapsed();
            if parent_has_exited(&mut self.parent_watch, self.inventory, now)
                && matches!(self.parent_watch.on_parent_exit, OnParentExit::Terminate)
            {
                self.apply_event(Event::ParentExited { at: now });
            }
            // The root command is gone but the group it owns is not. Drain the checkpoint socket
            // first so an acknowledgement already in flight is not lost, then ask the policy
            // machine to clean the survivors up. The machine latches its own intervention flag, so
            // samples of the departed root degrade to missing observations instead of failing the
            // supervisor closed.
            if self.root_outcome.is_some() && group_exists && !self.root_exit_applied {
                self.root_exit_applied = true;
                self.poll_checkpoint_channel();
                self.apply_event(Event::RootExited {
                    at: self.started.elapsed(),
                });
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
            && self.engine.policy().state() == PolicyState::CheckpointRequested
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
        self.escape_detected |= sample.escape_observed;
        if sample.sequence < MAX_SAMPLE_HISTORY_CAPACITY as u64 {
            self.record(
                JournalEntry::Sample(Box::new(window.clone())),
                JournalDurability::Buffered,
            );
        }
        self.apply_event(Event::Sample(policy_event));
    }

    fn apply_event(&mut self, event: Event) {
        let uncounted = opens_an_uncounted_shutdown(&event);
        if may_label_shutdown(&event, self.engine.policy().state())
            && let Some(reason) = shutdown_reason_for(&event)
        {
            self.shutdown_reason = Some(reason);
        }
        if let Event::CheckpointAck {
            at,
            authenticated: true,
            ..
        } = &event
        {
            self.checkpoint_status = CheckpointStatus::AcknowledgedUnverifiedDurability;
            self.checkpoint_at_ms = Some(duration_ms(*at));
        }
        let new_records = self.handle_event(event);
        self.intervention_started |= !uncounted && !new_records.is_empty();
        self.record_actuations(&new_records);
    }

    /// Apply one event to the policy machine, persisting every transition it causes, and return
    /// the intervention attempts that were executed for it.
    fn handle_event(&mut self, event: Event) -> Vec<InterventionRecord> {
        let event_cause = intervention_cause_for(&event);
        let error_diagnostic = supervisor_error_diagnostic(&event);
        let previous_attempts = self.engine.evidence().total_attempts;
        let final_footprint = observed_value(&self.final_footprint);
        let Self {
            engine,
            journal,
            sequence,
            intervention_cause,
            supervisor_error,
            ..
        } = self;
        let _ = engine.handle_with_transition_observer(event, |at, from, to| {
            // An emergency-band breach skips the graceful path entirely, so the cause has to be
            // latched on the way into `Emergency` as well or its KILL would carry no reason.
            if intervention_cause.is_none()
                && matches!(from, PolicyState::Normal | PolicyState::Warning)
                && matches!(
                    to,
                    PolicyState::CheckpointRequested
                        | PolicyState::Terminating
                        | PolicyState::Emergency
                )
            {
                *intervention_cause = event_cause;
            }
            if to == PolicyState::SupervisorError && supervisor_error.is_none() {
                *supervisor_error = Some(error_diagnostic);
            }
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
        let records = engine.evidence().records();
        let new_record_count = usize::try_from(new_attempts).unwrap_or(usize::MAX);
        let start = records.len().saturating_sub(new_record_count);
        records.iter().skip(start).copied().collect()
    }

    /// Record one signal per executed attempt, carrying the reason it was requested.
    fn record_actuations(&mut self, new_records: &[InterventionRecord]) {
        for record in new_records {
            let reason =
                actuation_reason(record.action, self.intervention_cause, self.shutdown_reason);
            // Every signal sent to the owned group is the shutdown in flight; a cooperative
            // checkpoint request is not, so it never relabels one.
            if !matches!(record.action, Actuation::Checkpoint { .. }) {
                self.shutdown_reason = reason;
            }
            if let Some(status) = checkpoint_progress(
                record.action,
                record.result,
                self.checkpoint_negotiation == CheckpointNegotiation::Ready,
            ) {
                self.checkpoint_status = status;
                self.checkpoint_at_ms = Some(duration_ms(record.requested_at));
            }
            if let Some(signal) = signal_record(
                record.requested_at,
                record.action,
                record.result,
                Some(checkpoint_signal_usr1().get()),
                reason,
            ) {
                self.record(JournalEntry::Signal(signal), JournalDurability::Sync);
            }
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
    /// The launching parent's identity, the watch decision, and the orphan time once observed.
    parent_watch: ParentWatchState,
    /// When the TERM grace of a parent-exit shutdown ends, relative to `started`. `Some` exactly
    /// while that shutdown is in flight, which is also what makes it own the completion.
    parent_shutdown_deadline: Option<Duration>,
    started: Instant,
    root_outcome: Option<RootOutcome>,
    final_footprint: Observed<u64>,
    escape_detected: bool,
}

impl ObserveRuntime {
    fn run(mut self) -> RuntimeResult {
        let completion = self.sample_until_exit();
        let ObserveFinal {
            outcome,
            kind,
            child_status,
            owned_group_survivors,
            diagnostic,
            relinquish,
        } = match completion {
            ObserveCompletion::Root { outcome, survivors } => ObserveFinal {
                outcome: supervisor_outcome(outcome),
                kind: terminal_kind(outcome),
                child_status: Some(ChildStatus::from(outcome)),
                owned_group_survivors: survivors.then_some(true),
                diagnostic: survivors.then(|| OBSERVE_SURVIVORS_DIAGNOSTIC.to_owned()),
                // Observe never signals the command, so survivors must outlive the run instead
                // of being killed when the owned process is dropped.
                relinquish: survivors,
            },
            ObserveCompletion::ParentExit {
                root_outcome,
                survivors,
            } => ObserveFinal {
                outcome: SupervisorOutcome::PolicyIntervention,
                kind: TerminalKind::PolicyIntervention,
                // The root's own status is still reported, even though the shutdown above it owns
                // the outcome; `None` only when the shutdown killed a root it never reaped.
                child_status: root_outcome.map(ChildStatus::from),
                owned_group_survivors: Some(survivors),
                diagnostic: Some(OBSERVE_PARENT_EXIT_DIAGNOSTIC.to_owned()),
                // The shutdown either killed the group or owns what is left of it, so cleanup on
                // drop stays armed: relinquishing here would re-orphan the survivors this
                // shutdown exists to remove.
                relinquish: false,
            },
            ObserveCompletion::ObservationFailure => ObserveFinal {
                outcome: SupervisorOutcome::SupervisorFailure,
                kind: TerminalKind::SupervisorFailure,
                child_status: None,
                owned_group_survivors: None,
                diagnostic: Some("footprint observation failed".to_owned()),
                relinquish: true,
            },
            ObserveCompletion::SupervisorFailure(diagnostic) => ObserveFinal {
                outcome: SupervisorOutcome::SupervisorFailure,
                kind: TerminalKind::SupervisorFailure,
                child_status: None,
                owned_group_survivors: None,
                diagnostic: Some(diagnostic),
                relinquish: true,
            },
        };
        // Relinquishing cleanup ownership before the remaining fallible work (journal writes,
        // finalization) runs means a panic there cannot unwind into kill-on-drop: survivors this
        // run decided to leave running are already outside the owned process's cleanup path.
        if relinquish {
            self.process.relinquish();
        }
        let terminal = TerminalOutcome {
            at_ms: duration_ms(self.started.elapsed()),
            kind,
            final_footprint_bytes: self.final_footprint.clone(),
            child_status,
            owned_group_survivors,
            parent_exited_at_ms: self.parent_watch.exited_at_ms,
        };
        Self::record_terminal(
            &mut self.journal,
            &mut self.sequence,
            &self.sampler,
            &self.report_samples,
            self.escape_detected,
            terminal,
        );
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
            // A parent-exit shutdown owns the completion for as long as it is in flight. The
            // root's death is then this shutdown's own doing, so reporting it as the outcome
            // would both hide the intervention and relinquish the survivors it exists to remove.
            if let Some(deadline) = self.parent_shutdown_deadline {
                if !group_exists {
                    return ObserveCompletion::ParentExit {
                        root_outcome: self.root_outcome,
                        survivors: false,
                    };
                }
                if self.started.elapsed() >= deadline {
                    match self.process.kill_group() {
                        Ok(result) => self.record_parent_exit_signal(9, result),
                        Err(error) => {
                            return ObserveCompletion::SupervisorFailure(error.to_string());
                        }
                    }
                    return ObserveCompletion::ParentExit {
                        root_outcome: self.root_outcome,
                        survivors: true,
                    };
                }
            } else {
                // The root's own exit ends the observation, whether or not owned-group members are
                // still running; observe reports the survivors rather than waiting for or
                // signalling them.
                if let Some(outcome) = self.root_outcome {
                    return ObserveCompletion::Root {
                        outcome,
                        survivors: group_exists,
                    };
                }
                // Deliberately after the completion check above: an observation whose command has
                // already finished must not manufacture a termination for a parent that died
                // alongside it.
                let now = self.started.elapsed();
                if parent_has_exited(&mut self.parent_watch, self.inventory, now)
                    && matches!(self.parent_watch.on_parent_exit, OnParentExit::Terminate)
                {
                    match self.process.terminate_group() {
                        Ok(result) => self.record_parent_exit_signal(15, result),
                        Err(error) => {
                            return ObserveCompletion::SupervisorFailure(error.to_string());
                        }
                    }
                    self.parent_shutdown_deadline = Some(now.saturating_add(TERM_GRACE));
                }
            }
            if let Err(diagnostic) = self.forward_terminal_signals() {
                return ObserveCompletion::SupervisorFailure(diagnostic);
            }
            if !self.observation_failed {
                self.sample_once();
            }
            // Measurement loss cannot end a shutdown already in flight: that would relinquish the
            // group mid-termination and report a supervisor failure for a run that intervened.
            // The bounded deadline above still ends the loop, sampling or not.
            if self.observation_failed && self.parent_shutdown_deadline.is_none() {
                return ObserveCompletion::ObservationFailure;
            }
            let now = self.started.elapsed();
            // A latched observation failure stops `sample_once`, which freezes the sampler's last
            // start: `delay_until_next` then saturates to zero for good, so honouring it would
            // spin the loop at full speed until the deadline. The configured interval keeps the
            // shutdown's remaining second at its normal cadence instead.
            let sample_delay = if self.observation_failed {
                self.sample_interval
            } else {
                self.sampler
                    .delay_until_next(now)
                    .unwrap_or(self.sample_interval)
            };
            let delay = self
                .parent_shutdown_deadline
                .map_or(sample_delay, |deadline| {
                    sample_delay.min(deadline.saturating_sub(now))
                });
            if !delay.is_zero() {
                thread::sleep(delay);
            }
        }
    }

    /// Record one signal the parent-exit shutdown delivered to the owned group.
    fn record_parent_exit_signal(&mut self, signal: u8, result: SignalResult) {
        record_resilient(
            &mut self.journal,
            &mut self.sequence,
            JournalEntry::Signal(SignalRecord {
                at_ms: duration_ms(self.started.elapsed()),
                signal,
                target: SignalTarget::OwnedProcessGroup,
                result,
                reason: Some(SignalReason::ParentExit),
            }),
            JournalDurability::Sync,
        );
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
            record_resilient(
                &mut self.journal,
                &mut self.sequence,
                JournalEntry::Signal(SignalRecord {
                    at_ms: duration_ms(self.started.elapsed()),
                    signal: delivered_signal,
                    target: SignalTarget::OwnedProcessGroup,
                    result,
                    reason: Some(SignalReason::ExternalSignal),
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
        self.escape_detected |= sample.escape_observed;
        if sample.sequence < MAX_SAMPLE_HISTORY_CAPACITY as u64 {
            record_resilient(
                &mut self.journal,
                &mut self.sequence,
                JournalEntry::Sample(Box::new(window.clone())),
                JournalDurability::Buffered,
            );
        }
        let previous_state = self.policy.state();
        let _ = self.policy.apply(Event::Sample(policy_event));
        let current_state = self.policy.state();
        if previous_state != current_state {
            record_resilient(
                &mut self.journal,
                &mut self.sequence,
                JournalEntry::Transition(TransitionRecord {
                    at_ms: duration_ms(processed_at),
                    from: previous_state,
                    to: current_state,
                    aggregate_footprint_bytes: observed_value(&window.aggregate_footprint_bytes),
                }),
                JournalDurability::Sync,
            );
        }
        self.observation_failed = current_state == PolicyState::SupervisorError;
    }

    // These take explicit field references rather than `&mut self` so `run` can call them after
    // relinquishing the owned process, which requires partially moving `self.process` out first.
    fn record_terminal(
        journal: &mut ResilientJournal<SecureJournal>,
        sequence: &mut u64,
        sampler: &FootprintSampler,
        report_samples: &VecDeque<mlx_guard_core::SampleWindow>,
        escape_detected: bool,
        outcome: TerminalOutcome,
    ) {
        Self::record_recent_sample_history(journal, sequence, sampler, report_samples);
        record_resilient(
            journal,
            sequence,
            JournalEntry::Checkpoint(CheckpointRecord {
                status: CheckpointStatus::NotNegotiated,
                at_ms: None,
            }),
            JournalDurability::Sync,
        );
        record_resilient(
            journal,
            sequence,
            JournalEntry::Escape(EscapeEvidence {
                detected: Observed::Available {
                    value: escape_detected,
                },
            }),
            JournalDurability::Buffered,
        );
        record_resilient(
            journal,
            sequence,
            JournalEntry::Outcome(outcome),
            JournalDurability::Sync,
        );
    }

    fn record_recent_sample_history(
        journal: &mut ResilientJournal<SecureJournal>,
        sequence: &mut u64,
        sampler: &FootprintSampler,
        report_samples: &VecDeque<mlx_guard_core::SampleWindow>,
    ) {
        if sampler
            .history()
            .next()
            .is_none_or(|sample| sample.sequence == 0)
        {
            return;
        }
        record_resilient(
            journal,
            sequence,
            JournalEntry::SampleHistoryReset,
            JournalDurability::Sync,
        );
        let recent_samples = report_samples.iter().cloned().collect::<Vec<_>>();
        for sample in recent_samples {
            record_resilient(
                journal,
                sequence,
                JournalEntry::Sample(Box::new(sample)),
                JournalDurability::Buffered,
            );
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ObserveCompletion {
    Root {
        outcome: RootOutcome,
        /// Whether owned-group members were still running when the root exited.
        survivors: bool,
    },
    /// The launching parent exited, so the run terminated the group it owns instead of going on
    /// observing an orphan.
    ParentExit {
        /// The root's own status, absent only when the shutdown killed a root it never reaped.
        root_outcome: Option<RootOutcome>,
        /// Whether owned-group members were still alive when the KILL decision was reached.
        survivors: bool,
    },
    ObservationFailure,
    SupervisorFailure(String),
}

/// Stderr diagnostic for an observe run the root ended while owned-group members were alive.
const OBSERVE_SURVIVORS_DIAGNOSTIC: &str =
    "observe ended at root exit; owned-group members were still running and were not signalled";

/// Stderr diagnostic for an observe run the launching parent's exit ended.
const OBSERVE_PARENT_EXIT_DIAGNOSTIC: &str =
    "observe ended because the launching parent exited; the owned group was terminated";

/// What an observe completion determines about the final report and process result.
struct ObserveFinal {
    outcome: SupervisorOutcome,
    kind: TerminalKind,
    child_status: Option<ChildStatus>,
    owned_group_survivors: Option<bool>,
    diagnostic: Option<String>,
    /// Whether the owned group must be left running instead of killed when the process drops.
    relinquish: bool,
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
        SupervisorOutcome::ChildExited(code) => TerminalKind::ChildExited { code },
        SupervisorOutcome::ChildSignaled(signal) => TerminalKind::ChildSignaled {
            signal: signal.get(),
        },
        _ => TerminalKind::SupervisorFailure,
    };
    // A child that exited before the supervisor could bind its identity still has a real status,
    // and it is the only status this run will ever observe.
    let child_status = match outcome {
        SupervisorOutcome::ChildExited(code) => Some(ChildStatus::Exited { code }),
        SupervisorOutcome::ChildSignaled(signal) => Some(ChildStatus::Signaled {
            signal: signal.get(),
        }),
        _ => None,
    };
    let terminal = TerminalOutcome {
        at_ms: 0,
        kind,
        final_footprint_bytes: Observed::Unknown,
        child_status,
        owned_group_survivors: None,
        parent_exited_at_ms: None,
    };
    let is_child_exit = matches!(
        outcome,
        SupervisorOutcome::ChildExited(_) | SupervisorOutcome::ChildSignaled(_)
    );
    if append_terminal(&mut journal, &mut sequence, false, terminal).is_err() {
        return RuntimeResult::failure(
            SupervisorOutcome::PartialArtifactFailure,
            "launch failed and its report could not be finalized",
        );
    }
    if is_child_exit {
        match journal.finalize() {
            Ok(artifacts) => RuntimeResult {
                outcome,
                stdout: artifacts.summary,
                stderr: String::new(),
            },
            Err(_) => RuntimeResult::failure(
                SupervisorOutcome::PartialArtifactFailure,
                "child exited but its report could not be finalized",
            ),
        }
    } else {
        if journal.finalize().is_err() {
            return RuntimeResult::failure(
                SupervisorOutcome::PartialArtifactFailure,
                "launch failed and its report could not be finalized",
            );
        }
        RuntimeResult::failure(outcome, diagnostic)
    }
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

fn observe_configuration(
    common: &CommonOptions,
    parent_watch: &ParentWatchState,
) -> ReportConfiguration {
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
        on_parent_exit: Some(parent_watch.on_parent_exit),
        parent_watch: Some(parent_watch.watch),
    }
}

fn run_configuration(options: &RunOptions, parent_watch: &ParentWatchState) -> ReportConfiguration {
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
        on_parent_exit: Some(parent_watch.on_parent_exit),
        parent_watch: Some(parent_watch.watch),
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
        checkpoint_timeout: Some(
            options
                .checkpoint_timeout
                .unwrap_or(DEFAULT_CHECKPOINT_TIMEOUT),
        ),
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

/// Return the shutdown reason an event establishes before the policy machine decides anything.
fn shutdown_reason_for(event: &Event) -> Option<SignalReason> {
    match event {
        Event::ExternalSignal { .. } => Some(SignalReason::ExternalSignal),
        Event::RootExited { .. } => Some(SignalReason::RootExitCleanup),
        Event::ParentExited { .. } => Some(SignalReason::ParentExit),
        _ => None,
    }
}

/// Whether an event may name the shutdown in flight before the policy machine decides anything.
///
/// The policy machine acts on a parent exit only from `Normal` or `Warning`; anywhere else the
/// event is suppressed, and a suppressed event must not relabel a shutdown another cause opened.
const fn may_label_shutdown(event: &Event, state: PolicyState) -> bool {
    match event {
        Event::ParentExited { .. } => matches!(state, PolicyState::Normal | PolicyState::Warning),
        _ => true,
    }
}

/// Whether an event opens a shutdown that must not claim the run's exit status.
///
/// A forwarded terminal signal and post-exit survivor cleanup are both asked for by the world
/// rather than chosen by this supervisor, so neither counts as an intervention; anything they
/// later escalate into still does. A parent exit is deliberately absent: acting on it is the
/// supervisor's own decision, and the run reports it as the intervention it is.
const fn opens_an_uncounted_shutdown(event: &Event) -> bool {
    matches!(
        event,
        Event::ExternalSignal { .. } | Event::RootExited { .. }
    )
}

/// Return the explicit limit whose breach an event can open an intervention for.
fn intervention_cause_for(event: &Event) -> Option<SignalReason> {
    match event {
        Event::Sample(_) => Some(SignalReason::Footprint),
        Event::Tick { .. } => Some(SignalReason::WallTime),
        _ => None,
    }
}

/// Name the failure that made the policy machine fail closed on this event.
fn supervisor_error_diagnostic(event: &Event) -> &'static str {
    match event {
        Event::Sample(_) => "footprint observation failed",
        Event::ActuationFailed { .. } => "signal delivery failed",
        Event::SupervisorFault { .. } => "supervisor fault",
        _ => "supervision failed closed",
    }
}

/// Derive why one executed actuation was requested.
///
/// A TERM's checkpoint disposition names every non-policy cause exactly; the remaining
/// dispositions escalate whichever explicit limit opened the intervention.
fn actuation_reason(
    action: Actuation,
    intervention_cause: Option<SignalReason>,
    shutdown_reason: Option<SignalReason>,
) -> Option<SignalReason> {
    match action {
        Actuation::Checkpoint { .. } => intervention_cause,
        Actuation::Term { checkpoint } => match checkpoint {
            CheckpointDisposition::SkippedRootExited => Some(SignalReason::RootExitCleanup),
            CheckpointDisposition::SkippedObservationFailure => {
                Some(SignalReason::ObservationFailure)
            }
            CheckpointDisposition::SkippedSupervisorFailure => Some(SignalReason::SupervisorFault),
            CheckpointDisposition::SkippedParentExited => Some(SignalReason::ParentExit),
            CheckpointDisposition::AcknowledgedUnverifiedDurability
            | CheckpointDisposition::TimedOut
            | CheckpointDisposition::SkippedCheckpointFailure
            | CheckpointDisposition::SkippedNotNegotiated => intervention_cause,
        },
        Actuation::ForwardSignal(_) => Some(SignalReason::ExternalSignal),
        // A KILL usually escalates a shutdown already under way and inherits its reason. An
        // emergency-band KILL escalates nothing, so it falls back to the limit that opened it.
        Actuation::Kill => shutdown_reason.or(intervention_cause),
    }
}

/// Return the checkpoint status one executed actuation proves, or `None` to leave it unchanged.
fn checkpoint_progress(
    action: Actuation,
    result: Result<ActuationOutcome, ActuationFailure>,
    negotiated: bool,
) -> Option<CheckpointStatus> {
    match action {
        Actuation::Checkpoint { .. } => match result {
            Ok(ActuationOutcome::Delivered) => Some(CheckpointStatus::RequestedUnverified),
            Ok(ActuationOutcome::ProcessMissing) => None,
            // A failed request cancels only a checkpoint that was actually negotiated.
            Err(_) => negotiated.then_some(CheckpointStatus::Cancelled),
        },
        Actuation::Term { checkpoint } => match checkpoint {
            CheckpointDisposition::TimedOut => Some(CheckpointStatus::TimedOut),
            // Cleaning up owned-group survivors after the root already exited reports nothing
            // about the cooperative endpoint, so the negotiated status stays exactly as it was.
            CheckpointDisposition::SkippedRootExited
            | CheckpointDisposition::AcknowledgedUnverifiedDurability
            | CheckpointDisposition::SkippedCheckpointFailure
            | CheckpointDisposition::SkippedObservationFailure
            | CheckpointDisposition::SkippedSupervisorFailure
            | CheckpointDisposition::SkippedNotNegotiated
            // A parent's death says nothing about the cooperative endpoint either, so the
            // negotiated status stays unchanged — this is the final value, not a placeholder.
            | CheckpointDisposition::SkippedParentExited => None,
        },
        Actuation::Kill | Actuation::ForwardSignal(_) => None,
    }
}

fn signal_record(
    at: Duration,
    action: Actuation,
    result: Result<ActuationOutcome, ActuationFailure>,
    checkpoint_signal: Option<u8>,
    reason: Option<SignalReason>,
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
        reason,
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

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Duration;

    use mlx_guard_core::{
        Actuation, ActuationFailure, ActuationKind, ActuationOutcome, CheckpointDisposition,
        CheckpointStatus, Event, NativeProcessInventory, OnParentExit, ParentWatch, PolicyState,
        SampleEvent, SignalNumber, SignalReason,
    };

    use super::{
        OnParentExitOption, ParentWatchState, actuation_reason, checkpoint_progress,
        establish_parent_watch, intervention_cause_for, may_label_shutdown,
        opens_an_uncounted_shutdown, parent_has_exited, shutdown_reason_for,
        supervisor_error_diagnostic,
    };

    fn sample() -> Event {
        Event::Sample(SampleEvent {
            captured_at: Duration::from_millis(10),
            processed_at: Duration::from_millis(11),
            window: Duration::from_millis(1),
            aggregate_bytes: Some(4096),
        })
    }

    fn tick() -> Event {
        Event::Tick {
            at: Duration::from_millis(11),
        }
    }

    fn root_exited() -> Event {
        Event::RootExited {
            at: Duration::from_millis(11),
        }
    }

    fn parent_exited() -> Event {
        Event::ParentExited {
            at: Duration::from_millis(11),
        }
    }

    fn external_signal() -> Event {
        Event::ExternalSignal {
            at: Duration::from_millis(11),
            signal: SignalNumber::new(15).unwrap(),
        }
    }

    #[test]
    fn a_signal_a_root_exit_or_a_parent_exit_opens_a_shutdown_before_the_policy_decides() {
        // Catches a sample or tick pre-labelling a shutdown the policy machine has not chosen yet,
        // and a parent exit reaching the report as somebody else's reason.
        assert_eq!(
            shutdown_reason_for(&external_signal()),
            Some(SignalReason::ExternalSignal)
        );
        assert_eq!(
            shutdown_reason_for(&root_exited()),
            Some(SignalReason::RootExitCleanup)
        );
        assert_eq!(
            shutdown_reason_for(&parent_exited()),
            Some(SignalReason::ParentExit)
        );
        assert_eq!(shutdown_reason_for(&sample()), None);
        assert_eq!(shutdown_reason_for(&tick()), None);
    }

    #[test]
    fn a_parent_exit_the_policy_machine_would_suppress_never_relabels_the_shutdown() {
        // Catches a parent exit arriving mid-escalation overwriting the reason that opened it: the
        // machine acts on it only from Normal or Warning, so only there may it name the shutdown.
        for state in [PolicyState::Normal, PolicyState::Warning] {
            assert!(may_label_shutdown(&parent_exited(), state), "{state:?}");
        }
        for state in [
            PolicyState::Observe,
            PolicyState::CheckpointRequested,
            PolicyState::Terminating,
            PolicyState::Emergency,
            PolicyState::Exited,
            PolicyState::SupervisorError,
        ] {
            assert!(!may_label_shutdown(&parent_exited(), state), "{state:?}");
            // Every other event keeps labelling from whatever state it arrives in.
            assert!(may_label_shutdown(&external_signal(), state), "{state:?}");
            assert!(may_label_shutdown(&root_exited(), state), "{state:?}");
            assert!(may_label_shutdown(&sample(), state), "{state:?}");
        }
    }

    #[test]
    fn only_a_shutdown_the_world_asked_for_leaves_the_run_status_unclaimed() {
        // Catches a parent exit being counted like a forwarded signal: the supervisor decides to
        // act on it, so it owns the run's exit status, while signals and cleanup never do.
        assert!(opens_an_uncounted_shutdown(&external_signal()));
        assert!(opens_an_uncounted_shutdown(&root_exited()));
        assert!(!opens_an_uncounted_shutdown(&parent_exited()));
        assert!(!opens_an_uncounted_shutdown(&sample()));
        assert!(!opens_an_uncounted_shutdown(&tick()));
    }

    #[test]
    fn a_nohup_style_launcher_stops_enforcement_without_stopping_detach_evidence() {
        // Catches the `!wants_detach` override disappearing from the hangup guard: detach enforces
        // nothing, so an inherited SIG_IGN disposition is no reason to stop watching for evidence.
        let inventory = NativeProcessInventory::new();

        let ignored = establish_parent_watch(inventory, true, OnParentExitOption::Terminate);
        assert_eq!(ignored.watch, ParentWatch::HangupIgnored);
        assert!(
            ignored.parent.is_none(),
            "a watch that is never checked must bind no identity"
        );
        assert_eq!(ignored.on_parent_exit, OnParentExit::Terminate);

        let detached = establish_parent_watch(inventory, true, OnParentExitOption::Detach);
        assert_eq!(detached.watch, ParentWatch::Detach);
        assert!(
            detached.parent.is_some(),
            "detach must bind the parent whose exit it records"
        );
        assert_eq!(detached.on_parent_exit, OnParentExit::Detach);
    }

    #[test]
    fn an_observable_parent_arms_exactly_the_watch_the_option_asked_for() {
        // Catches an enforcing watch that bound no identity — it could never fire — and either
        // option arming the other one's watch.
        //
        // The two remaining arms cannot be driven from here: `establish_parent_watch` reads this
        // process's real parent, and under a test runner that parent is neither pid 1 nor
        // uninspectable, so `ParentIsLaunchd` and `ParentUnobservable` have no in-process input
        // that reaches them.
        let inventory = NativeProcessInventory::new();

        let active = establish_parent_watch(inventory, false, OnParentExitOption::Terminate);
        assert_eq!(active.watch, ParentWatch::Active);
        assert!(
            active.parent.is_some(),
            "an enforcing watch must bind the identity it enforces against"
        );
        assert_eq!(active.on_parent_exit, OnParentExit::Terminate);
        assert_eq!(active.exited_at_ms, None);

        let detached = establish_parent_watch(inventory, false, OnParentExitOption::Detach);
        assert_eq!(detached.watch, ParentWatch::Detach);
        assert!(detached.parent.is_some());
        assert_eq!(detached.exited_at_ms, None);
    }

    #[test]
    fn a_measurement_opens_a_footprint_intervention_and_a_tick_opens_a_wall_one() {
        // Catches attributing every intervention to whichever limit was checked most recently.
        assert_eq!(
            intervention_cause_for(&sample()),
            Some(SignalReason::Footprint)
        );
        assert_eq!(
            intervention_cause_for(&tick()),
            Some(SignalReason::WallTime)
        );
        assert_eq!(intervention_cause_for(&root_exited()), None);
        assert_eq!(intervention_cause_for(&parent_exited()), None);
        assert_eq!(
            intervention_cause_for(&Event::SupervisorFault {
                at: Duration::from_millis(11),
            }),
            None
        );
    }

    #[test]
    fn every_way_of_failing_closed_names_the_failure_that_caused_it() {
        // Catches one generic diagnostic standing in for measurement, delivery, and fault failures.
        assert_eq!(
            supervisor_error_diagnostic(&sample()),
            "footprint observation failed"
        );
        assert_eq!(
            supervisor_error_diagnostic(&Event::ActuationFailed {
                at: Duration::from_millis(11),
                action: ActuationKind::Kill,
                failure: ActuationFailure::SignalFailed,
            }),
            "signal delivery failed"
        );
        assert_eq!(
            supervisor_error_diagnostic(&Event::SupervisorFault {
                at: Duration::from_millis(11),
            }),
            "supervisor fault"
        );
        assert_eq!(
            supervisor_error_diagnostic(&tick()),
            "supervision failed closed"
        );
    }

    #[test]
    fn a_term_reports_the_cause_its_checkpoint_disposition_names() {
        // Catches survivor cleanup, lost measurement, and supervisor faults all reading as a breach.
        let named = [
            (
                CheckpointDisposition::SkippedRootExited,
                SignalReason::RootExitCleanup,
            ),
            (
                CheckpointDisposition::SkippedObservationFailure,
                SignalReason::ObservationFailure,
            ),
            (
                CheckpointDisposition::SkippedSupervisorFailure,
                SignalReason::SupervisorFault,
            ),
            (
                CheckpointDisposition::SkippedParentExited,
                SignalReason::ParentExit,
            ),
        ];
        for (disposition, expected) in named {
            assert_eq!(
                actuation_reason(
                    Actuation::Term {
                        checkpoint: disposition
                    },
                    Some(SignalReason::WallTime),
                    None,
                ),
                Some(expected),
                "{disposition:?}"
            );
        }

        let escalating = [
            CheckpointDisposition::AcknowledgedUnverifiedDurability,
            CheckpointDisposition::TimedOut,
            CheckpointDisposition::SkippedCheckpointFailure,
            CheckpointDisposition::SkippedNotNegotiated,
        ];
        for disposition in escalating {
            assert_eq!(
                actuation_reason(
                    Actuation::Term {
                        checkpoint: disposition
                    },
                    Some(SignalReason::Footprint),
                    None,
                ),
                Some(SignalReason::Footprint),
                "{disposition:?}"
            );
        }
    }

    #[test]
    fn a_checkpoint_request_and_a_forwarded_signal_report_their_own_cause() {
        // Catches a cooperative request inheriting an unrelated shutdown reason.
        assert_eq!(
            actuation_reason(
                Actuation::Checkpoint {
                    request_id: 7,
                    overshoot_bytes: 1,
                    deadline_at: Duration::from_millis(20),
                },
                Some(SignalReason::Footprint),
                Some(SignalReason::ExternalSignal),
            ),
            Some(SignalReason::Footprint)
        );
        assert_eq!(
            actuation_reason(
                Actuation::ForwardSignal(SignalNumber::new(2).unwrap()),
                Some(SignalReason::Footprint),
                None,
            ),
            Some(SignalReason::ExternalSignal)
        );
    }

    #[test]
    fn a_kill_inherits_the_shutdown_it_escalates() {
        // Catches an escalation relabelling a shutdown that another signal already started.
        assert_eq!(
            actuation_reason(
                Actuation::Kill,
                Some(SignalReason::Footprint),
                Some(SignalReason::ExternalSignal),
            ),
            Some(SignalReason::ExternalSignal)
        );
        assert_eq!(
            actuation_reason(Actuation::Kill, None, Some(SignalReason::RootExitCleanup)),
            Some(SignalReason::RootExitCleanup)
        );
    }

    #[test]
    fn an_emergency_kill_is_attributed_to_the_footprint_that_caused_it() {
        // Catches the headline scenario, an immediate emergency-band kill, recording no reason at
        // all: it escalates no earlier signal, so its cause is the breach that opened it.
        assert_eq!(
            actuation_reason(Actuation::Kill, Some(SignalReason::Footprint), None),
            Some(SignalReason::Footprint)
        );
    }

    #[test]
    fn only_a_delivered_or_cancelled_request_moves_the_checkpoint_status() {
        // Catches a failed request cancelling a checkpoint that was never negotiated.
        let request = Actuation::Checkpoint {
            request_id: 7,
            overshoot_bytes: 1,
            deadline_at: Duration::from_millis(20),
        };
        assert_eq!(
            checkpoint_progress(request, Ok(ActuationOutcome::Delivered), true),
            Some(CheckpointStatus::RequestedUnverified)
        );
        assert_eq!(
            checkpoint_progress(request, Ok(ActuationOutcome::ProcessMissing), true),
            None
        );
        assert_eq!(
            checkpoint_progress(request, Err(ActuationFailure::CheckpointRejected), true),
            Some(CheckpointStatus::Cancelled)
        );
        assert_eq!(
            checkpoint_progress(request, Err(ActuationFailure::CheckpointUnavailable), false),
            None
        );
    }

    #[test]
    fn only_a_timed_out_term_moves_the_checkpoint_status_and_no_group_signal_does() {
        // Catches root-exit cleanup or a forwarded signal overwriting negotiated checkpoint state.
        assert_eq!(
            checkpoint_progress(
                Actuation::Term {
                    checkpoint: CheckpointDisposition::TimedOut,
                },
                Ok(ActuationOutcome::Delivered),
                true,
            ),
            Some(CheckpointStatus::TimedOut)
        );
        let unchanged = [
            CheckpointDisposition::SkippedRootExited,
            CheckpointDisposition::AcknowledgedUnverifiedDurability,
            CheckpointDisposition::SkippedCheckpointFailure,
            CheckpointDisposition::SkippedObservationFailure,
            CheckpointDisposition::SkippedSupervisorFailure,
            CheckpointDisposition::SkippedNotNegotiated,
            CheckpointDisposition::SkippedParentExited,
        ];
        for disposition in unchanged {
            assert_eq!(
                checkpoint_progress(
                    Actuation::Term {
                        checkpoint: disposition
                    },
                    Ok(ActuationOutcome::Delivered),
                    true,
                ),
                None,
                "{disposition:?}"
            );
        }
        assert_eq!(
            checkpoint_progress(Actuation::Kill, Ok(ActuationOutcome::Delivered), true),
            None
        );
        assert_eq!(
            checkpoint_progress(
                Actuation::ForwardSignal(SignalNumber::new(15).unwrap()),
                Ok(ActuationOutcome::Delivered),
                true,
            ),
            None
        );
    }

    #[test]
    fn a_ppid_match_alone_proves_the_parent_alive_without_inspecting_it() {
        // Catches a transient inspection hiccup (rusage/proc_pidinfo failing for one tick) being
        // read as the parent's death while `getppid()` still names it: on macOS reparenting only
        // ever happens after the real parent has exited, so the pid match alone is conclusive and
        // must short-circuit before any inspection, let alone latch a false exit off of one.
        let inventory = NativeProcessInventory;
        let real_parent_pid = i32::try_from(std::os::unix::process::parent_id())
            .expect("this test process's real ppid must fit i32");
        let parent = inventory
            .inspect(real_parent_pid)
            .expect("this test process's real, live parent must be inspectable")
            .identity;
        let mut state = ParentWatchState {
            parent: Some(parent),
            watch: ParentWatch::Active,
            on_parent_exit: OnParentExit::Terminate,
            exited_at_ms: None,
        };

        assert!(!parent_has_exited(
            &mut state,
            inventory,
            Duration::from_millis(1)
        ));
        assert!(
            state.exited_at_ms.is_none(),
            "a live parent must never latch an exit"
        );
    }

    #[test]
    fn a_changed_ppid_only_latches_once_the_recorded_identity_is_confirmed_gone() {
        // Catches the pre-filter's ppid mismatch alone being trusted as proof of death: it must
        // only ever open a confirmation step, and only a recorded identity that confirmation can
        // no longer see alive may latch the exit.
        let inventory = NativeProcessInventory;
        let mut child = Command::new("/bin/sleep")
            .arg("5")
            .spawn()
            .expect("a throwaway child process must spawn");
        let child_pid = i32::try_from(child.id()).expect("the throwaway child's pid must fit i32");
        let recorded = inventory
            .inspect(child_pid)
            .expect("the live throwaway child must be inspectable")
            .identity;
        child
            .kill()
            .expect("the throwaway child must accept SIGKILL");
        child.wait().expect("the throwaway child must be reaped");

        // `child_pid` is a distinct, freshly spawned pid: it cannot equal this test process's
        // real, unchanged ppid, so the pre-filter is guaranteed to fire and fall through to
        // confirmation instead of returning early on a pid match.
        let real_parent_pid = i32::try_from(std::os::unix::process::parent_id()).unwrap_or(0);
        assert_ne!(real_parent_pid, child_pid);

        let mut state = ParentWatchState {
            parent: Some(recorded),
            watch: ParentWatch::Active,
            on_parent_exit: OnParentExit::Terminate,
            exited_at_ms: None,
        };

        assert!(parent_has_exited(
            &mut state,
            inventory,
            Duration::from_millis(7)
        ));
        assert_eq!(state.exited_at_ms, Some(7));
    }
}
