//! What `shoal restore` prints.
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceName;

use crate::output::{agents::Agent, batch::Failure, outputs};
use shoal_protocol::{
    model::{Chat, PresentSpace, QueueEntry, RemovedSpace, Workspace},
    saved::SavedSummary,
};

/// A workspace's stopped commands, which Shoal reports rather than reruns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoppedCommands {
    pub workspace: SpaceName,
    /// Each command's arguments.
    pub commands: Vec<Vec<String>>,
}

/// A stopped agent a restore started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoredAgent {
    pub workspace: SpaceName,
    pub execution: ExecutionId,
    /// Whether it started in the background rather than where it stopped.
    pub background: bool,
    /// Whether it continues its session.
    pub reused: bool,
}

/// A workspace whose failed setup a restore retried and finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetUp {
    pub workspace: Workspace,
    /// The launch of the agent that waited for the setup, waiting in the
    /// agent queue or starting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<QueueEntry>,
}

/// What a restore that started agents at once did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StartedRestores {
    pub commands: Vec<StoppedCommands>,
    pub agents: Vec<RestoredAgent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set_up: Vec<SetUp>,
    /// The removed spaces it opened again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reopened: Vec<Reopened>,
    /// The agents whose interrupted turns it continued.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub continued: Vec<Agent>,
    pub failed: Vec<Failure>,
}

/// The executions a restore forgot instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgottenWork {
    pub forgotten: Vec<ExecutionId>,
    /// The removed spaces it opened again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reopened: Vec<Reopened>,
    pub failed: Vec<Failure>,
}

/// The restores a restore queued, the commands it reported, the saved chats
/// it restored at once, the workspaces it set up, the interrupted turns it
/// continued and the spaces it failed for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Queued {
    pub queued: Vec<QueueEntry>,
    pub commands: Vec<StoppedCommands>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set_up: Vec<SetUp>,
    /// The removed spaces it opened again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reopened: Vec<Reopened>,
    /// The agents whose interrupted turns it continued.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub continued: Vec<Agent>,
    pub failed: Vec<Failure>,
}

/// A space opened again from a removal record with its PRs linked, a
/// workspace set up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reopened {
    pub space: PresentSpace,
    pub removed: RemovedSpace,
    pub warnings: Vec<String>,
    /// Its stopped agents stay saved for a later `shoal restore`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub agents_saved: bool,
    /// The restores of its stopped agents, once queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<Queued>,
}

/// A workspace with stopped work or a last session, and what it saved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoppedWorkspace {
    pub workspace: Workspace,
    pub saved: SavedSummary,
}

/// What `shoal restore` without a space picks among: the workspaces with
/// stopped work, unfinished work first, the stopped chats and the removed
/// spaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Restorable {
    pub stopped: Vec<StoppedWorkspace>,
    pub chats: Vec<Chat>,
    pub removed: Vec<RemovedSpace>,
}

outputs!(
    StoppedCommands,
    SetUp,
    StartedRestores,
    ForgottenWork,
    Queued,
    Reopened,
    Restorable
);
