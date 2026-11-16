//! Explicit recovery: what to repair, and what an inspection found.
use shoal_protocol::recovery::{ReconcileOptions, Report};

use anyhow::Result;

use crate::connection::{Daemon, request};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    model::{Checkpoint, RestoredCheckpoint},
    protocol::RecoveryMethod,
};

/// Keep `workspace`'s HEAD and unsaved files as a checkpoint named `name`.
pub async fn checkpoint(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: Option<String>,
) -> Result<Checkpoint> {
    request(daemon, RecoveryMethod::Checkpoint { workspace, name }).await
}

/// Every checkpoint of `workspace`, read page by page; `None` when the
/// workspace has none to restore.
pub async fn checkpoints(
    daemon: &impl Daemon,
    workspace: &SpaceSelector,
) -> Result<Option<Vec<Checkpoint>>> {
    let mut checkpoints = Vec::new();
    loop {
        let after = checkpoints
            .last()
            .map(|checkpoint: &Checkpoint| checkpoint.number);
        let method = RecoveryMethod::Checkpoints {
            workspace: workspace.clone(),
            after,
        };
        let Some(page) = request::<Option<Vec<Checkpoint>>>(daemon, method).await? else {
            return Ok(None);
        };
        if page.is_empty() {
            return Ok(Some(checkpoints));
        }
        checkpoints.extend(page);
    }
}

/// Return `workspace` to its checkpoint `checkpoint`, keeping the current
/// state in a new one first.
pub async fn restore_checkpoint(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    checkpoint: CheckpointNumber,
) -> Result<RestoredCheckpoint> {
    let method = RecoveryMethod::RestoreCheckpoint {
        workspace,
        checkpoint,
    };
    request(daemon, method).await
}

/// Inspect `workspace`, or every workspace, and repair what `options` asks.
pub async fn reconcile(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
    options: ReconcileOptions,
) -> Result<Vec<Report>> {
    request(daemon, RecoveryMethod::Reconcile { workspace, options }).await
}
