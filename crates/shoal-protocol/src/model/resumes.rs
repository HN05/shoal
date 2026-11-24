//! Resuming a space's agent: how a request asks for it and what the daemon
//! did with it.
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;

/// How a space's agent resumes, as `shoal resume` asks for it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResume {
    /// Only the stopped agent or last session of this execution, without
    /// continuing an interrupted turn or a failed launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionId>,
    /// Start at once, regardless of the agent limits.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub now: bool,
    /// The resume names this space alone, so a last session, which holds
    /// no heavy slot, continues at once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alone: bool,
    /// What it changes in the permissions the agent was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<crate::config::permissions::PermissionChange>,
    /// The environment a restore starts with, which places it on the host
    /// server of the caller's terminal.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
}

/// What the daemon did with a request to resume a space's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ResumeOutcome {
    /// It continued the turn an interrupt ended of the space's agent.
    Continued {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<crate::agent::Agent>,
    },
    /// It queued the restores of the space's stopped agents and last
    /// sessions, or its failed launch, each waiting at its position or
    /// starting at once, and reports the stopped `commands` that no
    /// restored agent receives.
    Queued {
        entries: Vec<crate::model::QueueEntry>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        commands: Vec<Vec<String>>,
    },
    /// The space saved only stopped commands, which it reports, each as its
    /// arguments, rather than reruns.
    Commands { commands: Vec<Vec<String>> },
    /// Nothing of the space resumes, for `reason`.
    Nothing { reason: String },
}
