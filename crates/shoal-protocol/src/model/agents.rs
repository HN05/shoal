//! Running agents, their reported state and sessions.
use crate::state::AgentState;
use serde::{Deserialize, Serialize};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

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

/// Why protection stopped an agent, and what its automatic restore waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Protection {
    pub reason: String,
    /// Completes "once …"; recovery also waits for every other signal.
    pub resumes_when: String,
}

/// Why Shoal put a space's agent to sleep and what wakes it: an asleep
/// agent comes back on its own through the agent queue, unlike a stopped
/// one, which waits for `shoal resume`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asleep {
    pub reason: String,
    /// Completes "wakes once …".
    pub wakes_when: String,
    /// The leases the agent keeps while it sleeps, which a space's status
    /// shows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keeps: Vec<KeptLease>,
}

/// A lease an asleep agent keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptLease {
    pub kind: KeptKind,
    /// A simulator lease's name or a permit's pool; none for containers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// When Shoal stops it, for one that keeps something running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<crate::time::UnixTime>,
}

shoal_states::states!(
    /// What kind of lease an asleep agent keeps.
    KeptKind {
        Simulator => "simulator",
        Containers => "containers",
        Resource => "resource",
    }
);

/// A connected agent, or an execution whose wrapper is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningAgent {
    pub execution: ExecutionId,
    /// The workspace it runs in, or the chat it is the agent of.
    pub space_id: SpaceId,
    /// `None` for an execution whose wrapper is gone, which may have run an agent.
    pub agent: Option<crate::agent::Agent>,
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
    /// Why Shoal stopped it, while its wrapper waits to restore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<Protection>,
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

/// The session a workspace's agent reported last, which a new agent can
/// start from a fork of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// Named as `--agent` names it.
    pub agent: crate::agent::Agent,
    pub session: AgentSessionId,
}
