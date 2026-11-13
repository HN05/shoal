use crate::model::CheckpointNumber;
use crate::recovery::ReconcileOptions;
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

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
    /// What an existing space saved for `shoal restore`.
    Saved { space: SpaceSelector },
    /// What each visible space that saved anything saved, in brief.
    SavedSummaries,
    /// Inspect one workspace, or all, and with `options` repair what verifies.
    Reconcile {
        workspace: Option<SpaceSelector>,
        options: ReconcileOptions,
    },
}
