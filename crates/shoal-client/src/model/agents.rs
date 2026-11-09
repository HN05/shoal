//! Running agents, their reported state and sessions.
use crate::execution_ids::ExecutionId;
use crate::space_ids::SpaceId;
use crate::state::AgentState;
use serde::{Deserialize, Serialize};

/// The turn state a space's agent last reported, and since when.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub state: AgentState,
    pub since: crate::time::UnixTime,
    /// When the prompt cache of an agent idle since a turn ended expires,
    /// for an AI tool whose cache lifetime is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_expires_at: Option<crate::time::UnixTime>,
}

/// A connected agent, or an execution whose wrapper is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningAgent {
    pub execution: ExecutionId,
    /// The workspace it runs in, or the chat it is the agent of.
    pub space_id: SpaceId,
    /// `None` for an execution whose wrapper is gone, which may have run an agent.
    pub agent: Option<String>,
    /// Whether its wrapper can start its session again.
    pub restorable: bool,
    /// The turn state the agent reported last; unknown after a daemon restart
    /// until it reports again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<crate::state::AgentState>,
    /// Whether an interrupt ended its last turn, which `shoal resume`
    /// continues.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// Where it runs in a terminal host, when its wrapper runs in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::sessions::Origin>,
    /// The `shoal restart` runs a restart started it again for, including
    /// runs that joined another's restart, while this daemon runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarted_by: Vec<String>,
    /// The `shoal restart` of it in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<PendingRestart>,
    /// Why Shoal stopped it, while its wrapper waits to restore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<crate::protocol::Protection>,
    /// The executions it ran as before restarts started it in new wrappers,
    /// while this daemon runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarted_from: Vec<ExecutionId>,
}

impl RunningAgent {
    /// Whether it runs as `execution`, or ran as it before a restart
    /// started it in a new wrapper.
    pub fn ran_as(&self, execution: &ExecutionId) -> bool {
        self.execution == *execution || self.restarted_from.contains(execution)
    }
}

/// A `shoal restart` that has not started its agent again yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRestart {
    /// The run that asked for it, unless a daemon before this one took it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The other runs whose requests joined it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joined: Vec<String>,
}

/// The session a workspace's agent reported last, which a new agent can
/// start from a fork of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// Named as `--agent` names it.
    pub agent: String,
    pub session: String,
}
