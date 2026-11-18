//! What a chat's start leaves for its end: the chat it recorded, which a
//! failed start removes, and the scope a scoped process issued to act in
//! as that chat, which it holds until the start's failure, if any, is
//! ended, then revokes.
use std::sync::{Mutex, OnceLock, PoisonError};

use anyhow::Result;

use crate::{cli::context::Context, model::Chat};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;

/// The chat a start recorded and the scope it acts in as that chat.
#[derive(Default)]
pub(super) struct Recorded {
    chat: OnceLock<SpaceId>,
    scope: Mutex<Option<(SpaceId, ScopeToken)>>,
}

impl Recorded {
    /// What a start handed the `chat` its starter recorded, if any, has
    /// recorded so far: that chat.
    pub(super) fn handed(chat: Option<&SpaceId>) -> Self {
        let recorded = Self::default();
        if let Some(chat) = chat {
            recorded.record(chat);
        }
        recorded
    }

    /// Keep `chat` as the one this start recorded.
    pub(super) fn record(&self, chat: &SpaceId) {
        let _ = self.chat.set(chat.clone());
    }

    /// The chat this start recorded, once it did.
    pub(super) fn chat(&self) -> Option<&SpaceId> {
        self.chat.get()
    }

    /// Make a scoped process, which runs `chat`'s agent, act in a scope it
    /// issues for the chat before its starter, whose scope it may hold,
    /// learns that it runs and exits.
    pub(super) async fn act_in_chat_scope(&self, ctx: &Context, chat: &Chat) -> Result<()> {
        if !crate::env::is_scoped() {
            return Ok(());
        }
        let space = SpaceSelector::id(&chat.id);
        let token = crate::client::spaces::issue_scope(&ctx.paths, space).await?;
        crate::env::act_in_chat_scope(token.clone());
        *self.scope.lock().unwrap_or_else(PoisonError::into_inner) = Some((chat.id.clone(), token));
        Ok(())
    }

    /// Revoke the scope this process issued for its chat, once its start
    /// ended. A token it cannot revoke goes with the chat, as when the
    /// chat's removal ended its agent.
    pub(super) async fn leave_chat_scope(&self, ctx: &Context) {
        let held = self
            .scope
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((chat, token)) = held {
            let space = SpaceSelector::id(&chat);
            let _ = crate::client::spaces::revoke_scope(&ctx.paths, space, token).await;
        }
    }
}
