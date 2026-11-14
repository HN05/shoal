//! The daemon's answers to [`ChatMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    chats::NewChat,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
use crate::protocol::{Body, ChatMethod, ChatStart};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ChatMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ChatMethod::LaunchChat { start } => {
            if caller.is_none() {
                let (workspace, parent) = (start.workspace.as_ref(), start.parent.as_ref());
                (manager.ensure_parent_not_host(workspace, parent)).await?;
            }
            let held = (start.repository.as_ref(), start.workspace.as_ref());
            ensure_caller_holds(manager, caller, &start.narrowing, held).await?;
            Body::StartedChat(manager.launch_chat(*start, caller).await?)
        }
        ChatMethod::StartChat {
            repository,
            workspace,
            name,
            agent,
            mode,
            narrowing,
            origin,
            parent,
            priority,
        } => {
            if caller.is_none() {
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
            }
            let held = (repository.as_ref(), workspace.as_ref());
            ensure_caller_holds(manager, caller, &narrowing, held).await?;
            let priority = manager.given_priority(caller, priority).await?;
            let new = NewChat {
                name,
                agent,
                mode: mode.unwrap_or_else(crate::model::chat_mode),
                narrowing,
                origin,
                parent: manager.space_id(parent.as_ref()).await?,
                priority,
            };
            Body::Chat(
                manager
                    .start_new_chat(repository, workspace.as_ref(), new)
                    .await?,
            )
        }
        ChatMethod::RemoveChat { chat, children } => {
            Body::RemovedChat(manager.remove_chat(&chat, children).await?)
        }
        ChatMethod::ListChats => Body::Chats(manager.visible_chats(caller).await?),
        ChatMethod::RenameChat { chat, name } => {
            let id = manager.chat(&chat).await?.id;
            Body::RenamedChat(manager.rename_chat(id, name).await?)
        }
        ChatMethod::CheckChatParent {
            repository,
            workspace,
            parent,
        } => {
            if caller.is_none() {
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
            }
            let parent = manager.space_id(parent.as_ref()).await?;
            (manager.ensure_parent_takes(repository.as_ref(), parent.as_ref())).await?;
            Body::Ok
        }
        ChatMethod::ChatRoot => Body::ChatRoot(crate::protocol::ChatRoot {
            path: manager.prepare_chat_root().await?,
        }),
        ChatMethod::ChatScope { chat } => {
            let id = manager.chat(&chat).await?.id;
            Body::ScopeToken(crate::protocol::ChatScope {
                token: manager.chat_scope(&id).await?,
            })
        }
        ChatMethod::PlaceChat { chat, origin } => {
            manager.place_chat(&chat, origin).await?;
            Body::Ok
        }
    }))
}

/// Refuse the permissions `given` to a new chat in the repository or the
/// workspace of `place` where the scoped `caller` does not hold them.
async fn ensure_caller_holds(
    manager: &Manager,
    caller: Option<&Caller>,
    given: &crate::config::permissions::PermissionsConfig,
    place: (Option<&crate::repository::Selector>, Option<&SpaceSelector>),
) -> Result<()> {
    let Some(caller) = caller.filter(|_| !given.is_empty()) else {
        return Ok(());
    };
    let repository = match place {
        (_, Some(workspace)) => Some(manager.chat_workspace(workspace).await?.repository_id),
        (Some(repository), None) => Some(manager.repository(repository.clone()).await?.id),
        (None, None) => None,
    };
    let giver = Some(&caller.space_id);
    (manager.ensure_caller_holds(giver, given, repository.as_ref())).await
}

impl Scoped for ChatMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            ChatMethod::ListChats => None,
            // A chat's agent chooses its chat's name.
            ChatMethod::RenameChat { chat, .. } => Some(
                manager
                    .own_workspace(caller, chat, "chat processes rename only their own chat")
                    .await?,
            ),
            // A scoped caller starts chats only in its own tree, below its
            // own space unless it names one below it, in a worktree of its
            // tree, and runs each in the session it starts, with its scope.
            // A named parent is never the workspace the chat runs in; one
            // it leaves out is its own space, wherever the chat runs.
            ChatMethod::StartChat {
                parent, workspace, ..
            } => {
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
                manager.parent_below(caller, parent).await?;
                workspace.as_mut()
            }
            // A chat the daemon starts goes where one the caller recorded
            // itself would: the worker acts with the caller's scope, which
            // puts it below the caller's own space without a parent.
            ChatMethod::LaunchChat { start } => {
                let ChatStart {
                    parent, workspace, ..
                } = &mut **start;
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
                manager.parent_below(caller, &mut parent.clone()).await?;
                workspace.as_mut()
            }
            // Its check takes the parent as its start would.
            ChatMethod::CheckChatParent {
                parent, workspace, ..
            } => {
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
                manager.parent_below(caller, parent).await?;
                None
            }
            ChatMethod::ChatScope { chat } => Some(
                manager
                    .workspace_below(
                        caller,
                        chat,
                        "scoped processes start only the chats below their own space",
                    )
                    .await?,
            ),
            ChatMethod::PlaceChat { chat, .. } => Some(
                manager
                    .workspace_below(
                        caller,
                        chat,
                        "scoped processes place only the chats below their own space",
                    )
                    .await?,
            ),
            ChatMethod::RemoveChat { chat, .. } => Some(
                manager
                    .unheld_workspace_below(
                        caller,
                        chat,
                        "scoped processes remove only the chats below their own space",
                    )
                    .await?,
            ),
            ChatMethod::ChatRoot => None,
        })
    }
}
