//! The daemon's answers to [`ChatMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
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
            name,
            agent,
            pid,
            origin,
            hands_over,
        } => Body::Chat(
            manager
                .start_chat(repository, name, agent, pid, origin, hands_over)
                .await?,
        ),
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
        ChatMethod::DiscardChat { chat } => Body::Chat(manager.discard_saved_chat(&chat).await?),
        ChatMethod::MoveChat { chat, pid } => Body::Chat(manager.move_chat(&chat, pid).await?),
        ChatMethod::ListChats => Body::Chats(manager.visible_chats(caller).await?),
        ChatMethod::RenameChat { chat, name } => Body::Chat(manager.rename_chat(chat, name).await?),
        ChatMethod::CheckChatName { name } => {
            manager.check_chat_name(name).await?;
            Body::Ok
        }
        ChatMethod::FreeChatName { name } => Body::ChatName(manager.free_chat_name(name).await?),
        ChatMethod::ChatDone {
            chat,
            results,
            sender_chat,
        } => {
            let attacher = manager.attacher(caller, sender_chat.as_deref()).await;
            Body::Completion(manager.mark_chat_done(&chat, results, &attacher).await?)
        }
        ChatMethod::ChatUndone { chat } => Body::Completion(manager.undo_chat_done(&chat).await?),
    }))
}

impl Scoped for ChatMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            ChatMethod::ListChats => None,
            ChatMethod::StartChat { .. }
            | ChatMethod::MoveChat { .. }
            | ChatMethod::RestoreChat { .. }
            | ChatMethod::SaveChat { .. }
            | ChatMethod::DiscardChat { .. }
            | ChatMethod::RenameChat { .. }
            | ChatMethod::CheckChatName { .. }
            | ChatMethod::FreeChatName { .. }
            | ChatMethod::ChatDone { .. }
            | ChatMethod::ChatUndone { .. } => return Err(denied()),
        })
    }
}
