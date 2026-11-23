//! The daemon's answers to [`RecoveryMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::execution::recovery::Saved;
use crate::protocol::{self, Body, RecoveryMethod, SavedPiece};
use shoal_protocol::space_names::SpaceSelector;

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
            let saved = manager.agents.saved_work(&**manager, &space).await;
            return Ok(Answer::pages(pieces(saved), Body::SavedPieces));
        }
        RecoveryMethod::SavedSummaries => {
            let summaries = manager.agents.saved_summaries(&**manager, caller).await;
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

impl Checked for RecoveryMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::Checkpoints { .. } | Self::Saved { .. } => Some(TargetCheck::Space),
            Self::Checkpoint { .. } => Some(TargetCheck::CheckpointedWorkspace),
            Self::RestoreCheckpoint { .. } | Self::Reconcile { .. } => Some(TargetCheck::Workspace),
            Self::SavedSummaries => None,
        }
    }
}

/// Repairing workspaces is the `admin` permission's.
impl Scoped for RecoveryMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        if let RecoveryMethod::Reconcile { workspace, .. } = self {
            manager
                .coordination
                .ensure_selected_reached(manager, caller, workspace.as_mut())
                .await?;
            let repository = match workspace {
                Some(workspace) => {
                    manager
                        .coordination
                        .space_repository(manager, workspace)
                        .await?
                }
                None => None,
            };
            manager
                .coordination
                .administer(manager, caller, self, repository)
                .await?;
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
