use mlx_guard_core::{SignalNumber, SupervisorOutcome};

#[test]
fn typed_supervisor_outcomes_have_frozen_exit_codes() {
    // Catches accidental collisions among supervisor-owned failure classes.
    assert_eq!(SupervisorOutcome::InvalidConfiguration.exit_code(), 64);
    assert_eq!(SupervisorOutcome::SupervisorFailure.exit_code(), 70);
    assert_eq!(SupervisorOutcome::PartialArtifactFailure.exit_code(), 74);
    assert_eq!(SupervisorOutcome::PolicyIntervention.exit_code(), 75);
    assert_eq!(SupervisorOutcome::LaunchNotExecutable.exit_code(), 126);
    assert_eq!(SupervisorOutcome::LaunchNotFound.exit_code(), 127);
}

#[test]
fn child_exit_is_preserved_and_signal_uses_shell_convention() {
    // Catches remapping a workload's own exit status or reporting a signal as normal success.
    for code in [0, 1, 64, 255] {
        assert_eq!(SupervisorOutcome::ChildExited(code).exit_code(), code);
    }
    assert_eq!(
        SupervisorOutcome::ChildSignaled(SignalNumber::new(9).unwrap()).exit_code(),
        137
    );
    assert_eq!(
        SupervisorOutcome::ChildSignaled(SignalNumber::new(15).unwrap()).exit_code(),
        143
    );
}

#[test]
fn signal_number_rejects_non_signal_values() {
    // Catches zero or an overflowing pseudo-signal entering exit-code arithmetic.
    assert!(SignalNumber::new(0).is_none());
    assert!(SignalNumber::new(127).is_some());
    assert!(SignalNumber::new(128).is_none());
}
