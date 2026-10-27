//! The daemon's answers to [`RecoveryMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped, denied},
    workspace::Manager,
};
use crate::protocol::{Body, RecoveryMethod};

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
        RecoveryMethod::Reconcile { workspace, options } => Body::Reconciled(
            manager
                .reconcile_workspaces(workspace.as_deref(), options)
                .await?,
        ),
    }))
}

impl Scoped for RecoveryMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            RecoveryMethod::Checkpoint { workspace, .. }
            | RecoveryMethod::Checkpoints { workspace, .. }
            | RecoveryMethod::RestoreCheckpoint { workspace, .. } => Some(workspace),
            RecoveryMethod::Reconcile { .. } => return Err(denied()),
        })
    }
}
