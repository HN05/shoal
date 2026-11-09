//! Checkpoints of a workspace's HEAD and unsaved files.
use super::Workspace;
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use serde::{Deserialize, Serialize};

crate::state::states!(
    /// Why a checkpoint was taken.
    CheckpointCause {
        Manual => "manual",
        /// An agent exited or was stopped.
        AgentStopped => "agent_stopped",
        /// The state a restore to an earlier checkpoint replaced.
        Restore => "restore",
        Removal => "removal",
        /// What a copy of another workspace started with, which keeps every
        /// unsaved file whatever the archive limit.
        Start => "start",
    }
);

crate::ids::number!(
    /// The number of one of a workspace's checkpoints, which counts them
    /// from 1.
    CheckpointNumber(u64)
);

/// A workspace's HEAD and unsaved files, kept until `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub workspace_id: SpaceId,
    pub repository_id: RepositoryId,
    pub number: CheckpointNumber,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub cause: CheckpointCause,
    /// The branch checked out; `None` for a detached HEAD.
    pub branch: Option<String>,
    pub head: String,
    /// HEAD, or a commit on top of it with the unsaved files.
    pub commit: String,
    /// Unsaved files exceeded the archive limit and were not kept.
    pub files_omitted: bool,
    pub created_at: crate::time::UnixTime,
    pub expires_at: crate::time::UnixTime,
}

/// A workspace `shoal work restore` returned to one of its checkpoints, and the
/// checkpoint of the state it replaced.
#[derive(Debug, Serialize, Deserialize)]
pub struct RestoredCheckpoint {
    pub workspace: Workspace,
    pub checkpoint: Checkpoint,
    pub replaced: Checkpoint,
}
