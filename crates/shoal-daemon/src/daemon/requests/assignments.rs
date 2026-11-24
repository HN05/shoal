//! The daemon's answers to [`AssignmentMethod`] requests, who may send them,
//! and what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    stopped_work::PermissionTargets,
    target_checks::{Checked, TargetCheck},
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
            Body::Completion(manager.spaces.undo_space_done(&**manager, &space).await?)
        }
        AssignmentMethod::DiscardStoppedAgent { space, execution } => Body::DiscardedAgent(
            manager
                .agents
                .discard_space_agent(&**manager, &space, execution)
                .await?,
        ),
        AssignmentMethod::ChangeStoppedPermissions {
            agents,
            launches,
            change,
        } => {
            let targets = PermissionTargets {
                agents: &agents,
                launches: &launches,
            };
            (manager
                .agents
                .change_stopped_permissions(&**manager, targets, &change, caller))
            .await?;
            Body::Ok
        }
        AssignmentMethod::MarkReady { space, selection } => Body::ReadyOutcomes(
            manager
                .spaces
                .mark_ready(&**manager, &space, selection, caller)
                .await?
                .into(),
        ),
        AssignmentMethod::ClearReady {
            space,
            selection,
            draft,
        } => Body::ReviewMarks(
            manager
                .spaces
                .clear_ready(&**manager, &space, selection, draft)
                .await?,
        ),
        AssignmentMethod::MarkDone { space, done } => {
            let attacher = manager.spaces.attacher(&**manager, caller).await;
            Body::Completion(
                manager
                    .spaces
                    .mark_space_done(&**manager, &space, done, &attacher)
                    .await?,
            )
        }
    }))
}

impl Checked for AssignmentMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::MarkDone { .. } | Self::DiscardStoppedAgent { .. } => Some(TargetCheck::Space),
            Self::UndoDone { .. } => Some(TargetCheck::DoneSpace),
            Self::ClearReady { .. } => Some(TargetCheck::MarkedSpace),
            Self::MarkReady { .. } => Some(TargetCheck::ReadySpace),
            Self::ChangeStoppedPermissions { .. } => None,
        }
    }
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
            AssignmentMethod::DiscardStoppedAgent { space, .. } => Some(
                manager
                    .coordination
                    .workspace_below(
                        manager,
                        caller,
                        space,
                        "scoped processes discard stopped agents only in the spaces below theirs",
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
                    manager
                        .coordination
                        .stopped_below(manager, caller, &mut agent.workspace)
                        .await?;
                }
                for space in launches {
                    manager
                        .coordination
                        .stopped_below(manager, caller, space)
                        .await?;
                }
                None
            }
        })
    }
}
