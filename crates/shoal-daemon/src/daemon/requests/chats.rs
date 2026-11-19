//! The daemon's answers to [`ChatMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{Body, ChatMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ChatMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ChatMethod::CheckChatParent {
            repository,
            workspace,
            parent,
        } => {
            if caller.is_none() {
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
            }
            let parent = manager.parent_space_id(parent.as_ref()).await?;
            (manager.ensure_parent_takes(repository.as_ref(), parent.as_ref())).await?;
            Body::Ok
        }
        ChatMethod::ChatRoot => Body::ChatRoot(crate::protocol::ChatRoot {
            path: manager.prepare_chat_root().await?,
        }),
    }))
}

impl Scoped for ChatMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            // Its check takes the parent as a chat's launch would.
            ChatMethod::CheckChatParent {
                parent, workspace, ..
            } => {
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
                manager.parent_below(caller, parent).await?;
                None
            }
            ChatMethod::ChatRoot => None,
        })
    }
}
