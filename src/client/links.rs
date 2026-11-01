//! What links a workspace to issues, PRs and CI runs reports: opening one,
//! and the activity a wait returns.
use serde::{Deserialize, Serialize};

use crate::{
    client::{agent_messages::AgentMessage, approvals::ApprovalRequest, processes::Leftover},
    forge::{items::item::Opened, updates::Update},
};

pub mod issue;
pub mod pr;

/// What `issue open` or `pr open` did: opened or found the item, or
/// recorded it for the user's approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenOutcome {
    Proposed(Proposed),
    /// Boxed, as the larger by far.
    Opened(Box<Opened>),
}

/// An issue or PR waiting for the user's approval before Shoal opens it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposed {
    pub proposed: Box<ApprovalRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Updates {
    pub updates: Vec<Update>,
    /// Agent messages end a wait on their own, ahead of item activity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<AgentMessage>,
    /// Leftovers of the space its agent has not been told about, which
    /// only the hook after a tool call delivers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leftovers: Vec<Leftover>,
    pub timed_out: bool,
    /// A newer wait in the same workspace took over before this one received
    /// updates; they stay pending for the newer wait.
    #[serde(default)]
    pub superseded: bool,
}
