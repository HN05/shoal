//! The daemon's answers to [`ChatMethod`] requests, and who may send them.
use crate::client::space_ids::SpaceId;
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    chats::NewChat,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
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
            narrowing,
            pid,
            origin,
            hands_over,
            parent,
        } => {
            if caller.is_none() {
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
            }
            let new = NewChat {
                name,
                agent,
                mode: mode.unwrap_or_else(crate::model::chat_mode),
                narrowing,
                pid,
                origin,
                hands_over,
                parent: manager.space_id(parent.as_ref()).await?,
            };
            Body::Chat(
                manager
                    .start_new_chat(repository, workspace.as_ref(), new)
                    .await?,
            )
        }
        ChatMethod::RestoreChat {
            chat,
            pid,
            origin,
            hands_over,
        } => {
            let id = manager.chat(&chat).await?.id;
            Body::Chat(manager.restore_chat(&id, pid, origin, hands_over).await?)
        }
        ChatMethod::CheckChatRestore { .. } => Body::Ok,
        ChatMethod::SaveChat { chat, pid, error } => {
            let id = manager.chat(&chat).await?.id;
            let stopped_by = caller.map(|caller| &caller.space_id);
            manager
                .return_to_saved(&id, pid, &error, stopped_by)
                .await?;
            Body::Ok
        }
        ChatMethod::RemoveChat { chat, children } => {
            Body::RemovedChat(manager.remove_chat(&chat, children).await?)
        }
        ChatMethod::MoveChat { chat, pid } => {
            let id = manager.chat(&chat).await?.id;
            Body::Chat(manager.move_chat(&id, pid).await?)
        }
        ChatMethod::ListChats => Body::Chats(manager.visible_chats(caller).await?),
        ChatMethod::RenameChat { chat, name } => {
            let id = manager.chat(&chat).await?.id;
            Body::RenamedChat(manager.rename_chat(id, name).await?)
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
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
            }
            let parent = manager.space_id(parent.as_ref()).await?;
            (manager.ensure_parent_takes(repository.as_ref(), parent.as_ref())).await?;
            Body::Ok
        }
        ChatMethod::FreeChatName { name } => Body::ChatName(manager.free_chat_name(name).await?),
        ChatMethod::ChatRoot => Body::ChatRoot(crate::protocol::ChatRoot {
            path: manager.prepare_chat_root().await?,
        }),
        ChatMethod::ChatScope { chat } => {
            let id = manager.chat(&chat).await?.id;
            Body::ScopeToken(crate::protocol::ScopeToken {
                token: manager.chat_scope(&id).await?,
            })
        }
        ChatMethod::ChatAgents => {
            let agents = manager.running_chat_agents().await?;
            Body::RunningAgents(manager.visible_agents(caller, agents).await?)
        }
        ChatMethod::RestartChat { chat, join, run } => {
            let id = restarted_chat(manager, chat).await?;
            Body::RestartedPlace(match join {
                Some(joined) => manager.join_chat_restart(&id, joined.run, run).await?,
                None => {
                    let stopped_by = caller.map(|caller| caller.space_id.clone());
                    manager.restart_chat(&id, run, stopped_by).await?
                }
            })
        }
    }))
}

/// The ID of the chat a restart names, or of one that ended meanwhile,
/// which a joined restart answers for as it did; a joined restart names
/// its chat by ID.
async fn restarted_chat(manager: &Manager, chat: SpaceSelector) -> Result<SpaceId> {
    match (manager.chat(&chat).await, chat) {
        (Ok(chat), _) => Ok(chat.id),
        (Err(_), SpaceSelector::Id(id)) => Ok(id),
        (Err(_), SpaceSelector::IdOrName(id)) => Ok(SpaceId::from(id)),
        (Err(error), SpaceSelector::Number(_)) => Err(error),
    }
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
            // tree, and hands each to the session it starts, with its scope.
            // A named parent is never the workspace the chat runs in; one
            // it leaves out is its own space, wherever the chat runs.
            ChatMethod::StartChat {
                parent, workspace, ..
            } => {
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
                manager.parent_below(caller, parent).await?;
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
            // The handler narrows the list to the chats below the caller's
            // space, the ones it may restart.
            ChatMethod::ChatAgents => None,
            ChatMethod::RestartChat { chat, .. } => Some(
                manager
                    .workspace_below(
                        caller,
                        chat,
                        "scoped processes restart only the agents of the spaces below their own",
                    )
                    .await?,
            ),
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
            ChatMethod::CheckChatName { .. }
            | ChatMethod::FreeChatName { .. }
            | ChatMethod::ChatRoot => None,
            // A scoped caller restores a chat from the background place it
            // opens for it, with its scope, once its check passed here.
            ChatMethod::RestoreChat { chat, .. } | ChatMethod::CheckChatRestore { chat } => {
                Some(manager.stopped_chat_below(caller, chat).await?)
            }
        })
    }
}
