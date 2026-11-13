//! What `shoal resume` and `shoal restore` print.
use crate::execution_ids::ExecutionId;
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};

use crate::{
    model::{QueueEntry, RemovedWorkspace, Workspace},
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

/// A workspace whose failed setup a resume retried and finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetUp {
    pub workspace: Workspace,
    /// The launch of the agent that waited for the setup, waiting in the
    /// agent queue or starting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<QueueEntry>,
}

/// What a resume of several spaces did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Resumed {
    pub commands: Vec<StoppedCommands>,
    pub agents: Vec<ResumedAgent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set_up: Vec<SetUp>,
    pub failed: Vec<Failure>,
}

/// The executions a restore forgot instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgottenWork {
    pub forgotten: Vec<ExecutionId>,
    pub failed: Vec<Failure>,
}

/// The restores a resume queued, the commands it reported, the saved chats
/// it restored at once, the workspaces it set up and the spaces it failed
/// for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Queued {
    pub queued: Vec<QueueEntry>,
    pub commands: Vec<StoppedCommands>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set_up: Vec<SetUp>,
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

outputs!(
    StoppedCommands,
    SetUp,
    Resumed,
    ForgottenWork,
    Queued,
    Reopened
);
