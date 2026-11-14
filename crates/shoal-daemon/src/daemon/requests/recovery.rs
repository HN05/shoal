//! The daemon's answers to [`RecoveryMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
use crate::execution::recovery::Saved;
use crate::protocol::{self, Body, RecoveryMethod, SavedPiece};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: RecoveryMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        RecoveryMethod::Checkpoint { workspace, name } => {
            Body::Checkpoint(manager.take_checkpoint(&workspace, name).await?)
        }
        RecoveryMethod::Checkpoints { workspace, after } => {
            Body::Checkpoints(manager.checkpoints(&workspace, after).await?)
        }
        RecoveryMethod::RestoreCheckpoint {
            workspace,
            checkpoint,
        } => Body::RestoredCheckpoint(
            manager
                .restore_checkpoint(&workspace, checkpoint, caller)
                .await?,
        ),
        RecoveryMethod::Saved { space } => {
            let saved = manager.saved_work(&space).await;
            return Ok(Answer::pages(pieces(saved), Body::SavedPieces));
        }
        RecoveryMethod::SavedSummaries => {
            let root = manager.reach_root(caller).await?;
            let summaries = manager.saved_summaries(root.as_ref()).await;
            return Ok(Answer::pages(summaries, Body::SavedSummaries));
        }
        RecoveryMethod::Reconcile { workspace, options } => Body::Reconciled(
            manager
                .reconcile_workspaces(workspace.as_ref(), options)
                .await?,
        ),
    }))
}

/// `saved` as pieces of its JSON, each fitting a frame.
fn pieces(saved: Result<Saved>) -> Result<Vec<SavedPiece>> {
    let json = serde_json::to_string(&saved?)?;
    Ok(protocol::text_pieces(&json)
        .into_iter()
        .map(SavedPiece)
        .collect())
}

/// Repairing workspaces is the `admin` permission's.
impl Scoped for RecoveryMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        if let RecoveryMethod::Reconcile { .. } = self {
            manager.administer(caller, self, None).await?;
        }
        Ok(match self {
            RecoveryMethod::Checkpoint { workspace, .. }
            | RecoveryMethod::Checkpoints { workspace, .. }
            | RecoveryMethod::RestoreCheckpoint { workspace, .. }
            | RecoveryMethod::Saved { space: workspace } => Some(workspace),
            RecoveryMethod::Reconcile { workspace, .. } => workspace.as_mut(),
            RecoveryMethod::SavedSummaries => None,
        })
    }
}
