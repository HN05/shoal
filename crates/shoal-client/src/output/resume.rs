//! What `shoal resume` and `shoal restore` print.
use crate::execution_ids::ExecutionId;
use crate::repository_ids::RepositoryId;
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};

use crate::{
    model::{Chat, QueueEntry, RemovedWorkspace, Workspace},
    output::{batch::Failure, outputs},
};

/// A workspace's stopped commands, which Shoal reports rather than reruns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoppedCommands {
    pub workspace: SpaceName,
    /// Each command's arguments.
    pub commands: Vec<Vec<String>>,
}

/// A stopped agent a resume started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumedAgent {
    pub workspace: SpaceName,
    pub execution: ExecutionId,
    /// Whether it started in the background rather than where it stopped.
    pub background: bool,
    /// Whether it continues its session.
    pub reused: bool,
}

/// What a resume of several spaces did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Resumed {
    pub commands: Vec<StoppedCommands>,
    pub agents: Vec<ResumedAgent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<RestoredChat>,
    pub failed: Vec<Failure>,
}

/// The executions and saved chats a discard forgot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscardedWork {
    pub discarded: Vec<ExecutionId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<Chat>,
    pub failed: Vec<Failure>,
}

/// The restores a resume queued, the commands it reported, the saved chats
/// it restored at once and the spaces it failed for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Queued {
    pub queued: Vec<QueueEntry>,
    pub commands: Vec<StoppedCommands>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<RestoredChat>,
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

/// A saved chat a resume restored in the background.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoredChat {
    pub chat: SpaceName,
    pub agent: crate::agent::Agent,
    /// Its repository's ID; none for a chat without one.
    pub repository: Option<RepositoryId>,
    /// Where it runs and how to reach it, as the host that runs it says;
    /// none when an app runs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
}

outputs!(StoppedCommands, Resumed, DiscardedWork, Queued, Reopened);
