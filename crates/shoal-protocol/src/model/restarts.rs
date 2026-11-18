//! Restarting a space's agent so it runs its agent's current binary: how a
//! request asks for it and what the daemon did with it.
use serde::{Deserialize, Serialize};

/// How a space's agent restarts, as `shoal restart` asks for it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRestart {
    /// Only once its turn ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wait: bool,
    /// In its wrapper rather than a new one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_place: bool,
    /// One at a time with the other sequential restarts the daemon holds,
    /// each at least the admission stagger after the previous start.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sequential: bool,
    /// What it changes in the permissions the agent was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<crate::config::permissions::PermissionChange>,
}

/// What the daemon did with a request to restart a space's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RestartOutcome {
    /// The agent runs again, at `place` when its wrapper runs in a terminal
    /// host.
    Restarted {
        agent: crate::agent::Agent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        place: Option<crate::sessions::Origin>,
    },
    /// The agent restarts once what it `waits` for happens.
    Waits {
        agent: crate::agent::Agent,
        waits: RestartWait,
    },
    /// The agent does not restart, for `reason`.
    Skipped {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<crate::agent::Agent>,
        reason: String,
    },
}

/// What a restart the daemon took waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestartWait {
    /// The agent's turn to end.
    Turn,
    /// The stopped agent's remaining processes to exit, which `issue` keeps
    /// from being proven, until the user ends them.
    Processes { issue: String },
}
