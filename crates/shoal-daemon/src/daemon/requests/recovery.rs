//! The daemon's answers to [`RecoveryMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped, denied},
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
        RecoveryMethod::Saved { workspace } => {
            let saved = manager.saved_work(&workspace).await;
            return Ok(Answer::pages(pieces(saved), Body::SavedPieces));
        }
        RecoveryMethod::StoppedWorkspaces => {
            let root = manager.reach_root(caller).await?;
            let stopped = manager.stopped_workspaces(root.as_ref()).await;
            return Ok(Answer::pages(stopped, Body::StoppedWorkspaces));
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

impl Scoped for RecoveryMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            RecoveryMethod::Checkpoint { workspace, .. }
            | RecoveryMethod::Checkpoints { workspace, .. }
            | RecoveryMethod::RestoreCheckpoint { workspace, .. }
            | RecoveryMethod::Saved { workspace } => Some(workspace),
            RecoveryMethod::StoppedWorkspaces => None,
            RecoveryMethod::Reconcile { .. } => return Err(denied()),
        })
    }
}
