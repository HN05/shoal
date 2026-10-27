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
        ChatMethod::WaitChatMessages { chat } => {
            let manager = manager.clone();
            return Ok(Answer::wait(async move {
                Ok(Body::PrUpdates(manager.wait_chat_messages(&chat).await?))
            }));
        }
        ChatMethod::StartChat {
            repository,
            name,
            agent,
            pid,
        } => Body::Chat(manager.start_chat(repository, name, agent, pid).await?),
        ChatMethod::MoveChat { chat, pid } => Body::Chat(manager.move_chat(&chat, pid).await?),
        ChatMethod::ListChats => Body::Chats(if caller.is_some() {
            Vec::new()
        } else {
            manager.chats().await?
        }),
        ChatMethod::CheckChatName { name } => {
            manager.check_chat_name(name).await?;
            Body::Ok
        }
        ChatMethod::FreeChatName { name } => Body::ChatName(manager.free_chat_name(name).await?),
        ChatMethod::ChatUpdates { chat } => Body::PrUpdates(manager.chat_updates(&chat).await?),
        ChatMethod::AcknowledgeChatMessages { chat, ids } => {
            manager.mark_chat_messages_delivered(&chat, ids).await?;
            Body::Ok
        }
    }))
}

impl Scoped for ChatMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            ChatMethod::ListChats => None,
            ChatMethod::StartChat { .. }
            | ChatMethod::MoveChat { .. }
            | ChatMethod::CheckChatName { .. }
            | ChatMethod::FreeChatName { .. }
            | ChatMethod::ChatUpdates { .. }
            | ChatMethod::WaitChatMessages { .. }
            | ChatMethod::AcknowledgeChatMessages { .. } => return Err(denied()),
        })
    }
}
