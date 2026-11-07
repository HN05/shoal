use crate::client::recovery::ReconcileOptions;
use crate::client::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests that keep, restore and repair workspace state: checkpoints and reconciliation.
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
        after: Option<u64>,
    },
    /// Return a workspace to one of its checkpoints.
    RestoreCheckpoint {
        workspace: SpaceSelector,
        checkpoint: u64,
    },
    /// Inspect one workspace, or all, and with `options` repair what verifies.
    Reconcile {
        workspace: Option<SpaceSelector>,
        options: ReconcileOptions,
    },
}
