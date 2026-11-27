//! What ended an agent that stopped rather than exited with a status of its
//! own. `docs/design/agent-lifecycle.md` says which ends leave it stopped
//! and which put it to sleep.
use serde::{Deserialize, Serialize};

/// What ended an agent that did not exit on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopCause {
    /// Idle sleep: the agent waited for a user who was away.
    Idle,
    /// Load, memory or disk space protection.
    Protection,
    /// Queued work needed the idle agent's slot or memory.
    MakingRoom,
    /// Its turn ended while its space was blocked.
    Blocked,
    /// A limit or an API error ended its turn, which Shoal retries.
    TurnRetry,
    /// `shoal restart`.
    Restart,
    /// A signal Shoal did not send.
    Signal {
        signal: EndSignal,
        reached: SignalTarget,
    },
    /// The kernel's OOM killer killed the agent.
    OomKill,
    /// Its machine crashed, lost power or never woke from a suspend.
    MachineDown,
    SystemShutdown,
    /// The Shoal daemon stopped.
    DaemonStop,
    /// The machine handed its work over as the user walked away or paused
    /// it, or as it went to sleep.
    Handover,
    /// `shoal stop`.
    Stop,
    /// `shoal queue cancel` of its restore.
    QueueCancel,
    /// Its space moved to another machine, or another machine took it over.
    Move,
    /// A repair of its workspace.
    Repair,
    /// Its chat became a workspace, or another agent tool took its work
    /// over.
    Conversion,
    /// Its space was removed.
    Removal,
}

impl std::fmt::Display for StopCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Signal { signal, reached } => return write!(f, "{signal} to the {reached}"),
            Self::Idle => "idle sleep",
            Self::Protection => "protection",
            Self::MakingRoom => "making room",
            Self::Blocked => "blocked",
            Self::TurnRetry => "turn retry",
            Self::Restart => "shoal restart",
            Self::OomKill => "OOM kill",
            Self::MachineDown => "machine down",
            Self::SystemShutdown => "system shutdown",
            Self::DaemonStop => "daemon stop",
            Self::Handover => "handover",
            Self::Stop => "shoal stop",
            Self::QueueCancel => "shoal queue cancel",
            Self::Move => "move",
            Self::Repair => "repair",
            Self::Conversion => "conversion",
            Self::Removal => "removal",
        };
        f.write_str(name)
    }
}

shoal_states::states!(
    /// A signal that ends an agent unasked.
    EndSignal {
        Hangup => "SIGHUP",
        Terminate => "SIGTERM",
        Kill => "SIGKILL",
    }
);

shoal_states::states!(
    /// Which process a signal that ended an agent reached first: the
    /// wrapper, or the agent without it.
    SignalTarget {
        Wrapper => "wrapper",
        Agent => "agent",
    }
);

#[cfg(test)]
mod tests;
