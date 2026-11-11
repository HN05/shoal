//! Blocked chats: once a chat's agent ends a turn while its progress
//! records what blocks it, and its session can be resumed, Shoal saves the
//! chat to come back on its own,
//! which waits until nothing blocks it, then restores it in a background
//! place as a saved chat's automatic restore does.
use anyhow::Result;

use crate::{
    daemon::{log, workspace::Manager},
    model::Chat,
    state::AgentState,
};

impl Manager {
    /// Save each running chat whose agent ended its turn while its space is
    /// blocked. A failure for one chat does not skip the others.
    pub(super) async fn save_blocked_chats(&self) -> Result<()> {
        for chat in self.chats().await? {
            if self.stops_blocked(&chat).await?
                && let Err(error) = self.save_blocked_chat(&chat.id).await
            {
                log!("blocked chat {} not saved: {error:#}", chat.id);
            }
        }
        Ok(())
    }

    /// Whether `chat` runs, its agent idle, while its space is blocked, and
    /// its restore would continue its session rather than start a new one.
    async fn stops_blocked(&self, chat: &Chat) -> Result<bool> {
        let idle = (chat.agent_state).is_some_and(|status| status.state == AgentState::Idle);
        Ok(chat.restore.is_none()
            && idle
            && self.space_blocked(&chat.id).await?
            && self.chat_resumes(chat).await?)
    }
}
