//! The cost the agent queue weighs for a CI job and what stopped a job's
//! runner, which the runner, the agents and the daemon share.

/// What a CI job is expected to use: the resident memory its earlier runs
/// reached, `None` before it has run, when it counts as an agent's start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JobCost {
    pub memory: Option<u64>,
}

/// What stopped a job's runner, as Shoal's log names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStop {
    /// `shoal stop` of its workspace.
    Stop,
    /// `shoal daemon stop`.
    DaemonStop,
    /// The stop before a system shutdown.
    SystemShutdown,
    /// The machine going to sleep.
    Sleep,
    /// The user pausing the machine.
    Pause,
    /// The removal of its workspace.
    Removal,
    /// Its runner outliving its finished run.
    Overrun,
    /// Load protection, for the pressure it names.
    Overload(&'static str),
    /// A signal ending its wrapper's session, as a logout or a shutdown
    /// sends.
    Signal(shoal_protocol::protocol::SessionSignal),
}

impl std::fmt::Display for JobStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stop => f.write_str("shoal stop"),
            Self::DaemonStop => f.write_str("shoal daemon stop"),
            Self::SystemShutdown => f.write_str("the system shutting down"),
            Self::Sleep => f.write_str("the machine going to sleep"),
            Self::Pause => f.write_str("the machine's pause"),
            Self::Removal => f.write_str("its workspace's removal"),
            Self::Overrun => f.write_str("its runner outliving its finished run"),
            Self::Overload(pressure) => f.write_str(pressure),
            Self::Signal(signal) => write!(f, "{signal} to its wrapper"),
        }
    }
}
