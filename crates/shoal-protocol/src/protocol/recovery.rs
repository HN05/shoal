use super::{Forwarding, Target};
use crate::recovery::ReconcileOptions;
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};
use shoal_ids::checkpoint_numbers::CheckpointNumber;

/// A piece of the JSON of a workspace's saved work, which the client joins:
/// a saved command's arguments alone may outgrow a frame.
#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SavedPiece(pub String);

/// Requests that keep, restore and repair workspace state: checkpoints,
/// saved work and reconciliation.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryMethod {
    /// Keep the workspace's HEAD and unsaved files in a new checkpoint.
    Checkpoint {
        workspace: SpaceSelector,
        name: Option<String>,
    },
    /// A page of an existing workspace's checkpoints numbered after `after`;
    /// none when no workspace matches.
    Checkpoints {
        workspace: SpaceSelector,
        after: Option<CheckpointNumber>,
    },
    /// Return a workspace to one of its checkpoints.
    RestoreCheckpoint {
        workspace: SpaceSelector,
        checkpoint: CheckpointNumber,
    },
    /// What an existing space saved for `shoal resume`.
    Saved { space: SpaceSelector },
    /// What each visible space that saved anything saved, in brief.
    SavedSummaries,
    /// Inspect one workspace, or all, and with `options` repair what verifies.
    Reconcile {
        workspace: Option<SpaceSelector>,
        options: ReconcileOptions,
    },
}

impl RecoveryMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::Checkpoints { .. } | Self::Saved { .. } | Self::SavedSummaries => true,
            Self::Checkpoint { .. } | Self::RestoreCheckpoint { .. } | Self::Reconcile { .. } => {
                false
            }
        }
    }
}

impl RecoveryMethod {
    /// Each reads or changes what the workspace keeps; a summary of every
    /// space is a listing of the caller's machine.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::Checkpoint { .. }
            | Self::Checkpoints { .. }
            | Self::RestoreCheckpoint { .. }
            | Self::Saved { .. }
            | Self::Reconcile { .. } => Forwarding::Online,
            Self::SavedSummaries => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::Checkpoint { workspace, .. }
            | Self::Checkpoints { workspace, .. }
            | Self::RestoreCheckpoint { workspace, .. }
            | Self::Saved { space: workspace } => Some(Target::Space(workspace)),
            Self::Reconcile { workspace, .. } => workspace.as_ref().map(Target::Space),
            Self::SavedSummaries => None,
        }
    }
}
