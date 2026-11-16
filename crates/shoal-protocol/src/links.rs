//! What links a workspace to issues, PRs and CI runs reports: opening one,
//! and the activity a wait returns.
use serde::{Deserialize, Serialize};

use crate::{
    agent_messages::AgentMessage,
    approvals::ApprovalRequest,
    forge::{items::item::Opened, updates::Update},
    processes::Leftover,
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

/// What linking an item reports beside recording it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Linked {
    /// Why the issue's or PR's title could not be read, which leaves the
    /// link without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_error: Option<String>,
    /// What assigning an issue on the forge reports: who else it is
    /// assigned to, or why it was not assigned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// What unlinking items reports beside removing the links: why an issue's
/// assignee stays on the forge.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Unlinked {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Whether a push may go ahead, or waits for the user's approval of the
/// request recorded for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushOutcome {
    Direct,
    Proposed(Proposed),
}

/// Whether the `push` permission holds back the push of the workspace's
/// branch that `shoal pr open` would run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BranchPush {
    pub held: bool,
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
