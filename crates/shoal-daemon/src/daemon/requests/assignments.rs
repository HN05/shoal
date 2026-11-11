//! The daemon's answers to [`AssignmentMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
use crate::protocol::{AssignmentMethod, Body};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: AssignmentMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        AssignmentMethod::UndoDone { space } => {
            Body::Completion(manager.undo_space_done(&space).await?)
        }
        AssignmentMethod::DiscardStoppedWork {
            workspace,
            execution,
        } => Body::Discarded(manager.discard_stopped_work(&workspace, execution).await?),
        AssignmentMethod::ChangeStoppedPermissions {
            agents,
            chats,
            change,
        } => {
            let giver = caller.map(|caller| &caller.space_id);
            (manager.change_stopped_permissions(&agents, &chats, &change, giver)).await?;
            Body::Ok
        }
        AssignmentMethod::MarkReady { space, selection } => {
            // Only a workspace process's mark waits for a review.
            let gated = caller.is_some();
            Body::ReadyOutcomes(manager.mark_ready(&space, selection, gated).await?.into())
        }
        AssignmentMethod::ClearReady {
            space,
            selection,
            draft,
        } => Body::ReviewMarks(manager.clear_ready(&space, selection, draft).await?),
        AssignmentMethod::MarkDone {
            space,
            done,
            sender_chat,
        } => {
            let attacher = manager.attacher(caller, sender_chat.as_ref()).await;
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
            AssignmentMethod::DiscardStoppedWork { workspace, .. } => Some(
                manager
                    .workspace_below(
                        caller,
                        workspace,
                        "workspace processes discard stopped work only in the workspaces below theirs",
                    )
                    .await?,
            ),
            // Every agent and chat is checked before the handler changes any.
            AssignmentMethod::ChangeStoppedPermissions { agents, chats, .. } => {
                for agent in agents {
                    (manager.stopped_below(caller, &mut agent.workspace, &agent.execution)).await?;
                }
                for chat in chats {
                    manager.stopped_chat_below(caller, chat).await?;
                }
                None
            }
        })
    }
}
