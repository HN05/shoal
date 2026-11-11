//! Blocked chats: once a chat's agent ends a turn while its progress
//! records what blocks it, with no work in flight and a session that can be
//! resumed, Shoal saves the chat to come back on its own, which waits until
//! nothing blocks it, then restores it in a background place as a saved
//! chat's automatic restore does.
use anyhow::Result;

use super::{processes, recorded_process};
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

    /// Whether `chat` runs, its agent idle with no work in flight, while its
    /// space is blocked, and its restore would continue its session rather
    /// than start a new one.
    async fn stops_blocked(&self, chat: &Chat) -> Result<bool> {
        let idle = (chat.agent_state).is_some_and(|status| status.state == AgentState::Idle);
        Ok(chat.restore.is_none()
            && idle
            && self.space_blocked(&chat.id).await?
            && self.chat_resumes(chat).await?
            && !self.chat_works_in_background(chat).await?)
    }

    /// Whether the running `chat` has work in flight outside its agent's
    /// process group, such as a background task its last turn started.
    async fn chat_works_in_background(&self, chat: &Chat) -> Result<bool> {
        let id = chat.id.clone();
        let Some(agent) = self.store.run(move |db| recorded_process(db, &id)).await? else {
            return Ok(false);
        };
        let wrappers = self.execution_wrappers().await?;
        processes::runs_outside_group(&chat.id, &agent, &wrappers).await
    }
}
