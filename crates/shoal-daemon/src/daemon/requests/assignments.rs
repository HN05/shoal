//! The daemon's answers to [`AssignmentMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    stopped_work::PermissionTargets,
};
use crate::protocol::{AssignmentMethod, Body};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: AssignmentMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        AssignmentMethod::UndoDone { space } => {
            Body::Completion(manager.undo_space_done(&space).await?)
        }
        AssignmentMethod::ForgetStoppedWork {
            workspace,
            execution,
        } => Body::Forgotten(manager.forget_stopped_work(&workspace, execution).await?),
        AssignmentMethod::ChangeStoppedPermissions {
            agents,
            launches,
            change,
        } => {
            let targets = PermissionTargets {
                agents: &agents,
                launches: &launches,
            };
            (manager.change_stopped_permissions(targets, &change, caller)).await?;
            Body::Ok
        }
        AssignmentMethod::MarkReady { space, selection } => {
            Body::ReadyOutcomes(manager.mark_ready(&space, selection, caller).await?.into())
        }
        AssignmentMethod::ClearReady {
            space,
            selection,
            draft,
        } => Body::ReviewMarks(manager.clear_ready(&space, selection, draft).await?),
        AssignmentMethod::MarkDone { space, done } => {
            let attacher = manager.attacher(caller).await;
            Body::Completion(manager.mark_space_done(&space, done, &attacher).await?)
        }
    }))
}

impl Scoped for AssignmentMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            AssignmentMethod::MarkDone { space, .. }
            | AssignmentMethod::UndoDone { space }
            | AssignmentMethod::MarkReady { space, .. }
            | AssignmentMethod::ClearReady { space, .. } => Some(space),
            AssignmentMethod::ForgetStoppedWork { workspace, .. } => Some(
                manager
                    .workspace_below(
                        caller,
                        workspace,
                        "workspace processes forget stopped work only in the workspaces below theirs",
                    )
                    .await?,
            ),
            // Every agent and launch is checked before the handler changes
            // any.
            AssignmentMethod::ChangeStoppedPermissions {
                agents, launches, ..
            } => {
                // A launch waiting for a failed setup or a failed launch is
                // a space's restore, as a stopped agent is.
                for agent in agents {
                    manager.stopped_below(caller, &mut agent.workspace).await?;
                }
                for space in launches {
                    manager.stopped_below(caller, space).await?;
                }
                None
            }
        })
    }
}
