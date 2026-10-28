//! The daemon's answers to [`AssignmentMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{AssignmentMethod, Body};

pub(super) async fn handle(manager: &Arc<Manager>, method: AssignmentMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        AssignmentMethod::WorkspaceUndone { workspace } => {
            Body::Completion(manager.undo_done(&workspace).await?)
        }
        AssignmentMethod::DiscardStoppedWork {
            workspace,
            execution,
        } => Body::Discarded(manager.discard_stopped_work(&workspace, execution).await?),
        AssignmentMethod::MarkReady {
            workspace,
            selection,
        } => Body::ReviewMarks(manager.mark_ready(&workspace, selection).await?),
        AssignmentMethod::ClearReady {
            workspace,
            selection,
            draft,
        } => Body::ReviewMarks(manager.clear_ready(&workspace, selection, draft).await?),
        AssignmentMethod::WorkspaceDone { workspace, cleanup } => {
            Body::Completion(manager.mark_done(&workspace, cleanup).await?)
        }
    }))
}

impl Scoped for AssignmentMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            AssignmentMethod::WorkspaceDone { workspace, .. }
            | AssignmentMethod::WorkspaceUndone { workspace }
            | AssignmentMethod::MarkReady { workspace, .. }
            | AssignmentMethod::ClearReady { workspace, .. } => Some(workspace),
            AssignmentMethod::DiscardStoppedWork { workspace, .. } => Some(
                manager
                    .workspace_below(
                        caller,
                        workspace,
                        "workspace processes discard stopped work only in the workspaces below theirs",
                    )
                    .await?,
            ),
        })
    }
}
