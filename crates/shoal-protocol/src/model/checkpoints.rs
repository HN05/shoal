//! Checkpoints of a workspace's HEAD and unsaved files.
use super::Workspace;
use serde::{Deserialize, Serialize};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::{BranchName, CommitId};

shoal_states::states!(
    /// Why a checkpoint was taken.
    CheckpointCause {
        Manual => "manual",
        /// An agent exited or was stopped.
        AgentStopped => "agent_stopped",
        /// The state a restore to an earlier checkpoint replaced.
        Restore => "restore",
        Removal => "removal",
        /// What a copy of another workspace started with.
        Start => "start",
        /// The state a refresh replaced with its source's files again.
        Refresh => "refresh",
    }
);

impl CheckpointCause {
    /// Whether the archive limit caps the unsaved files the checkpoint
    /// keeps: only for an agent's exit or stop, which nobody asked for and
    /// whose files stay in the worktree. Any other checkpoint is asked for
    /// or may hold their last copy.
    pub fn capped(self) -> bool {
        self == Self::AgentStopped
    }
}

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
    pub branch: Option<BranchName>,
    pub head: CommitId,
    /// HEAD, or a commit on top of it with the unsaved files.
    pub commit: CommitId,
    /// Unsaved files were not kept: they exceeded the archive limit of a
    /// capped checkpoint, or included a nested repository or a changed
    /// submodule.
    pub files_omitted: bool,
    pub created_at: crate::time::UnixTime,
    pub expires_at: crate::time::UnixTime,
}

/// A workspace `shoal restore` returned to one of its checkpoints, and the
/// checkpoint of the state it replaced.
#[derive(Debug, Serialize, Deserialize)]
pub struct RestoredCheckpoint {
    pub workspace: Workspace,
    pub checkpoint: Checkpoint,
    pub replaced: Checkpoint,
}
