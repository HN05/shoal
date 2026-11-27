//! What ended an agent that stopped, which its `agent_stopped` event
//! records and `shoal log` prints.
use serde::{Deserialize, Serialize};
use shoal_states::states;
use std::fmt;

/// What stopped an agent. `docs/design/agent-lifecycle.md` says which
/// causes leave it asleep and which leave it stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopCause {
    /// Shoal put it to sleep, idle while the user was away.
    IdleSleep,
    /// Shoal put it to sleep, idle, to make room for queued work.
    MakingRoom,
    /// Its turn ended while its space was blocked.
    Blocked,
    /// Its turn failed, and it sleeps until its retry.
    TurnRetry,
    /// Memory, CPU or disk space protection, for `reason`.
    Protection {
        reason: String,
    },
    Restart,
    /// A signal Shoal did not send ended its session.
    Signal(SessionEnd),
    /// The kernel's OOM killer killed the agent.
    OomKill,
    /// Its machine crashed, lost power or never woke from a suspend.
    MachineDown,
    /// The system or the daemon shut down.
    Shutdown,
    UserStop,
    /// Its space was removed.
    Removal,
    /// Another request of Shoal or the user, for `reason`, such as a move
    /// to another machine or a chat's conversion.
    Requested {
        reason: String,
    },
}

/// A signal that ended an agent's session, and what received it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnd {
    pub signal: EndingSignal,
    pub reached: SignalTarget,
}

states!(
    /// A signal that ends an agent's session.
    EndingSignal {
        Hangup => "SIGHUP",
        Terminate => "SIGTERM",
        Kill => "SIGKILL",
    }
);

states!(
    /// What a signal that ended an agent's session reached first: its
    /// wrapper, which saved the agent before stopping it, or the agent alone.
    SignalTarget {
        Wrapper => "wrapper",
        Agent => "agent",
    }
);

impl StopCause {
    /// The cause of a stop the signal that `end`ed the agent's session made:
    /// the OOM killer's for a SIGKILL while its scope counted `oom_kills`.
    pub fn ended(end: SessionEnd, oom_kills: u64) -> Self {
        match end.signal {
            EndingSignal::Kill if oom_kills > 0 => Self::OomKill,
            _ => Self::Signal(end),
        }
    }
}

impl fmt::Display for StopCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdleSleep => f.write_str("idle sleep"),
            Self::MakingRoom => f.write_str("making room for queued work"),
            Self::Blocked => f.write_str("its space is blocked"),
            Self::TurnRetry => f.write_str("waiting to retry its turn"),
            Self::Protection { reason } => write!(f, "protection: {reason}"),
            Self::Restart => f.write_str("shoal restart"),
            Self::Signal(end) => write!(f, "{} to its {}", end.signal, end.reached),
            Self::OomKill => f.write_str("OOM kill"),
            Self::MachineDown => f.write_str("its machine went down"),
            Self::Shutdown => f.write_str("shutdown"),
            Self::UserStop => f.write_str("shoal stop"),
            Self::Removal => f.write_str("removal"),
            Self::Requested { reason } => f.write_str(reason),
        }
    }
}

#[cfg(test)]
mod tests;
