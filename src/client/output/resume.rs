//! What `shoal resume` and `shoal work restore` print.
use serde::{Deserialize, Serialize};

use crate::{
    client::output::{batch::Failure, outputs},
    model::{QueueEntry, RemovedWorkspace, Workspace},
};

/// A workspace's stopped commands, which Shoal reports rather than reruns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoppedCommands {
    pub workspace: String,
    /// Each command's arguments.
    pub commands: Vec<Vec<String>>,
}

/// A stopped agent a resume started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumedAgent {
    pub workspace: String,
    pub execution: String,
    /// Whether it started in the background rather than where it stopped.
    pub background: bool,
    /// Whether it continues its session.
    pub reused: bool,
}

/// What a resume of several workspaces did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resumed {
    pub commands: Vec<StoppedCommands>,
    pub agents: Vec<ResumedAgent>,
    pub failed: Vec<Failure>,
}

/// The executions a discard forgot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscardedWork {
    pub discarded: Vec<String>,
    pub failed: Vec<Failure>,
}

/// The restores a resume queued, the commands it reported and the
/// workspaces it failed for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queued {
    pub queued: Vec<QueueEntry>,
    pub commands: Vec<StoppedCommands>,
    pub failed: Vec<Failure>,
}

/// A workspace opened again from a removal record, set up with its PRs
/// linked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reopened {
    pub workspace: Workspace,
    pub removed: RemovedWorkspace,
    pub warnings: Vec<String>,
    /// The restores of its stopped agents, once queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<Queued>,
}

outputs!(StoppedCommands, Resumed, DiscardedWork, Queued, Reopened);
