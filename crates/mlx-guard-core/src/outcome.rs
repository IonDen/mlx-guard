/// A real POSIX signal number that can be represented by the shell exit convention.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalNumber(u8);

impl SignalNumber {
    /// Construct a signal number in `1..=127`.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value == 0 || value > 127 {
            None
        } else {
            Some(Self(value))
        }
    }

    /// Return the validated numeric signal.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Top-level process outcome used to derive the CLI process status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorOutcome {
    /// The child returned normally. Its status is preserved even if it matches a supervisor code.
    ChildExited(u8),
    /// The child died from a signal. The CLI uses the conventional `128 + signal` status.
    ChildSignaled(SignalNumber),
    /// The configured executable did not exist.
    LaunchNotFound,
    /// The executable existed but could not be invoked.
    LaunchNotExecutable,
    /// CLI or policy configuration was invalid before launch.
    InvalidConfiguration,
    /// The explicit memory or wall policy caused an intervention.
    PolicyIntervention,
    /// The supervisor itself could not uphold its contract.
    SupervisorFailure,
    /// Safety action completed, but a partial artifact could not be durably preserved.
    PartialArtifactFailure,
}

impl SupervisorOutcome {
    /// Return the frozen v0.1 process exit code.
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::ChildExited(code) => code,
            Self::ChildSignaled(signal) => 128 + signal.get(),
            Self::InvalidConfiguration => 64,
            Self::SupervisorFailure => 70,
            Self::PartialArtifactFailure => 74,
            Self::PolicyIntervention => 75,
            Self::LaunchNotExecutable => 126,
            Self::LaunchNotFound => 127,
        }
    }
}
