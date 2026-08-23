//! Deterministic mapping from one finished supervision run to its reported result.
//!
//! The run loop knows four independent facts by the time it stops: the root command's own status,
//! whether a counted policy intervention was attempted, whether the policy machine failed closed,
//! and whether a supervisor-owned I/O operation failed. Exactly one of them owns the process exit
//! status, and the order never depends on how the run reached its end.

use mlx_guard_core::{ChildStatus, RootOutcome, SupervisorOutcome, TerminalKind};

/// Everything one finished supervision run knows about how it ended.
pub(crate) struct CompletionInputs {
    /// The root command's own waited status, when the supervisor observed one.
    pub root_outcome: Option<RootOutcome>,
    /// Whether a counted intervention attempt was made against the owned group.
    pub intervention_started: bool,
    /// Diagnostic latched when the policy machine entered `PolicyState::SupervisorError`.
    pub supervisor_error: Option<&'static str>,
    /// Diagnostic for a supervisor-owned I/O failure that ended the run.
    pub supervisor_failure: Option<String>,
}

/// The reported result of one supervision run.
pub(crate) struct Completion {
    /// Process status the CLI returns.
    pub outcome: SupervisorOutcome,
    /// Terminal kind recorded in the final report.
    pub kind: TerminalKind,
    /// The root command's own status, recorded whatever else owns the outcome.
    pub child_status: Option<ChildStatus>,
    /// Operator-facing diagnostic, present only for a supervisor failure.
    pub diagnostic: Option<String>,
}

/// Derive the reported result by a fixed precedence, independent of event order.
///
/// A supervisor I/O failure outranks a failed-closed policy machine, which outranks an
/// intervention, which outranks the root command's own status. The child's status is reported
/// whenever it was observed, even when something above it owns the outcome.
pub(crate) fn complete(inputs: CompletionInputs) -> Completion {
    let child_status = inputs.root_outcome.map(ChildStatus::from);
    let (outcome, kind, diagnostic) = if let Some(diagnostic) = inputs.supervisor_failure {
        (
            SupervisorOutcome::SupervisorFailure,
            TerminalKind::SupervisorFailure,
            Some(diagnostic),
        )
    } else if let Some(diagnostic) = inputs.supervisor_error {
        (
            SupervisorOutcome::SupervisorFailure,
            TerminalKind::SupervisorFailure,
            Some(diagnostic.to_owned()),
        )
    } else if inputs.intervention_started {
        (
            SupervisorOutcome::PolicyIntervention,
            TerminalKind::PolicyIntervention,
            None,
        )
    } else if let Some(root_outcome) = inputs.root_outcome {
        (
            supervisor_outcome(root_outcome),
            terminal_kind(root_outcome),
            None,
        )
    } else {
        (
            SupervisorOutcome::SupervisorFailure,
            TerminalKind::SupervisorFailure,
            Some("supervision ended without an observed root status".to_owned()),
        )
    };
    Completion {
        outcome,
        kind,
        child_status,
        diagnostic,
    }
}

/// Map an observed root status to the process status the CLI returns.
pub(crate) fn supervisor_outcome(outcome: RootOutcome) -> SupervisorOutcome {
    match outcome {
        RootOutcome::Exited(code) => SupervisorOutcome::ChildExited(code),
        RootOutcome::Signaled(signal) => SupervisorOutcome::ChildSignaled(signal),
    }
}

/// Map an observed root status to its recorded terminal kind.
pub(crate) fn terminal_kind(outcome: RootOutcome) -> TerminalKind {
    match outcome {
        RootOutcome::Exited(code) => TerminalKind::ChildExited { code },
        RootOutcome::Signaled(signal) => TerminalKind::ChildSignaled {
            signal: signal.get(),
        },
    }
}

#[cfg(test)]
mod tests {
    use mlx_guard_core::{ChildStatus, RootOutcome, SignalNumber, SupervisorOutcome, TerminalKind};

    use super::{CompletionInputs, complete};

    #[test]
    fn root_status_wins_when_nothing_intervened() {
        // Catches a quiet run inventing a supervisor result instead of forwarding the child's.
        let completion = complete(CompletionInputs {
            root_outcome: Some(RootOutcome::Exited(23)),
            intervention_started: false,
            supervisor_error: None,
            supervisor_failure: None,
        });

        assert_eq!(completion.outcome, SupervisorOutcome::ChildExited(23));
        assert_eq!(completion.kind, TerminalKind::ChildExited { code: 23 });
        assert_eq!(
            completion.child_status,
            Some(ChildStatus::Exited { code: 23 })
        );
        assert_eq!(completion.diagnostic, None);
    }

    #[test]
    fn intervention_owns_the_result_but_child_status_survives() {
        // Catches an intervention either losing the exit code it forced or hiding the real status.
        let completion = complete(CompletionInputs {
            root_outcome: Some(RootOutcome::Exited(3)),
            intervention_started: true,
            supervisor_error: None,
            supervisor_failure: None,
        });

        assert_eq!(completion.outcome, SupervisorOutcome::PolicyIntervention);
        assert_eq!(completion.kind, TerminalKind::PolicyIntervention);
        assert_eq!(
            completion.child_status,
            Some(ChildStatus::Exited { code: 3 })
        );
        assert_eq!(completion.diagnostic, None);
    }

    #[test]
    fn supervisor_error_beats_intervention_and_names_its_reason() {
        // Catches reporting failed supervision as a deliberate policy intervention.
        let completion = complete(CompletionInputs {
            root_outcome: Some(RootOutcome::Signaled(SignalNumber::new(9).unwrap())),
            intervention_started: true,
            supervisor_error: Some("footprint observation failed"),
            supervisor_failure: None,
        });

        assert_eq!(completion.outcome, SupervisorOutcome::SupervisorFailure);
        assert_eq!(completion.kind, TerminalKind::SupervisorFailure);
        assert_eq!(
            completion.child_status,
            Some(ChildStatus::Signaled { signal: 9 })
        );
        assert_eq!(
            completion.diagnostic.as_deref(),
            Some("footprint observation failed")
        );
    }

    #[test]
    fn io_failure_beats_everything() {
        // Catches a lost report or journal being masked by whatever the policy did first.
        let completion = complete(CompletionInputs {
            root_outcome: Some(RootOutcome::Exited(0)),
            intervention_started: true,
            supervisor_error: Some("footprint observation failed"),
            supervisor_failure: Some("owned process group query failed".to_owned()),
        });

        assert_eq!(completion.outcome, SupervisorOutcome::SupervisorFailure);
        assert_eq!(completion.kind, TerminalKind::SupervisorFailure);
        assert_eq!(
            completion.child_status,
            Some(ChildStatus::Exited { code: 0 })
        );
        assert_eq!(
            completion.diagnostic.as_deref(),
            Some("owned process group query failed")
        );
    }

    #[test]
    fn a_run_that_never_observed_a_root_status_reports_supervisor_failure() {
        // Catches the unobserved-root fallback claiming a child result it never measured.
        let completion = complete(CompletionInputs {
            root_outcome: None,
            intervention_started: false,
            supervisor_error: None,
            supervisor_failure: None,
        });

        assert_eq!(completion.outcome, SupervisorOutcome::SupervisorFailure);
        assert_eq!(completion.kind, TerminalKind::SupervisorFailure);
        assert_eq!(completion.child_status, None);
        assert!(completion.diagnostic.is_some());
    }
}
