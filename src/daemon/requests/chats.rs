//! The daemon's answers to [`ChatMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    chats::NewChat,
    scope::{Caller, Scoped, denied},
    workspace::Manager,
};
use crate::protocol::{Body, ChatMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ChatMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ChatMethod::StartChat {
            repository,
            workspace,
            name,
            agent,
            mode,
            pid,
            origin,
            hands_over,
            parent,
        } => {
            let new = NewChat {
                name,
                agent,
                mode: mode.unwrap_or_else(crate::model::chat_mode),
                pid,
                origin,
                hands_over,
                parent,
            };
            let workspace = workspace.as_deref();
            if caller.is_none() {
                (manager.ensure_parent_not_host(workspace, new.parent.as_deref())).await?;
            }
            Body::Chat(manager.start_new_chat(repository, workspace, new).await?)
        }
        ChatMethod::RestoreChat {
            chat,
            pid,
            origin,
            hands_over,
        } => Body::Chat(manager.restore_chat(&chat, pid, origin, hands_over).await?),
        ChatMethod::SaveChat { chat, pid, error } => {
            manager.return_to_saved(&chat, pid, &error).await?;
            Body::Ok
        }
        ChatMethod::RemoveChat { chat, children } => {
            Body::RemovedChat(manager.remove_chat(&chat, children).await?)
        }
        ChatMethod::MoveChat { chat, pid } => Body::Chat(manager.move_chat(&chat, pid).await?),
        ChatMethod::ListChats => Body::Chats(manager.visible_chats(caller).await?),
        ChatMethod::RenameChat { chat, name } => {
            Body::RenamedChat(manager.rename_chat(chat, name).await?)
        }
        ChatMethod::CheckChatName { name } => {
            manager.check_chat_name(name).await?;
            Body::Ok
        }
        ChatMethod::CheckChatParent {
            repository,
            workspace,
            parent,
        } => {
            if caller.is_none() {
                (manager.ensure_parent_not_host(workspace.as_deref(), parent.as_deref())).await?;
            }
            (manager.ensure_parent_takes(repository.as_ref(), parent.as_deref())).await?;
            Body::Ok
        }
        ChatMethod::FreeChatName { name } => Body::ChatName(manager.free_chat_name(name).await?),
        ChatMethod::ChatScope { chat } => Body::ScopeToken(crate::protocol::ScopeToken {
            token: manager.chat_scope(&chat).await?,
        }),
        ChatMethod::ChatAgents => Body::RunningAgents(manager.running_chat_agents().await?),
        ChatMethod::RestartChat { chat, join, run } => Body::RestartedPlace(match join {
            Some(joined) => manager.join_chat_restart(&chat, joined.run, run).await?,
            None => manager.restart_chat(&chat, run).await?,
        }),
    }))
}

impl Scoped for ChatMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
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
            // tree, and hands each to the session it starts, with its scope.
            // A named parent is never the workspace the chat runs in; one
            // it leaves out is its own space, wherever the chat runs.
            ChatMethod::StartChat {
                parent, workspace, ..
            } => {
                (manager.ensure_parent_not_host(workspace.as_deref(), parent.as_deref())).await?;
                manager.parent_below(caller, parent).await?;
                workspace.as_mut()
            }
            // Its check takes the parent as its start would.
            ChatMethod::CheckChatParent {
                parent, workspace, ..
            } => {
                (manager.ensure_parent_not_host(workspace.as_deref(), parent.as_deref())).await?;
                manager.parent_below(caller, parent).await?;
                None
            }
            ChatMethod::MoveChat { chat, .. }
            | ChatMethod::SaveChat { chat, .. }
            | ChatMethod::ChatScope { chat } => Some(
                manager
                    .workspace_below(
                        caller,
                        chat,
                        "scoped processes start only the chats below their own space",
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
            ChatMethod::CheckChatName { .. } | ChatMethod::FreeChatName { .. } => None,
            ChatMethod::RestoreChat { .. }
            | ChatMethod::ChatAgents
            | ChatMethod::RestartChat { .. } => return Err(denied()),
        })
    }
}
