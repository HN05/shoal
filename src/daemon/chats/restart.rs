//! `shoal restart` of a chat: the daemon saves the chat's session and stops
//! its agent, as a save before a daemon stop does, then restores it as
//! `shoal resume` does, in a background place where a restored agent opens.
//! The restart goes on without the requests that asked for it, and answers
//! each once the chat runs again, with the place it runs in.
use std::sync::Arc;

use anyhow::{Result, anyhow};

use super::restores::resume;
use crate::{
    daemon::{
        log,
        workspace::{Manager, restarted},
    },
    model::{Chat, RunningAgent},
    sessions::Origin,
};

impl Manager {
    /// Restart the agent of the running chat `id` for the `shoal restart`
    /// `run`, or join the restart in progress, returning the place it runs
    /// in again.
    pub(crate) async fn restart_chat(
        self: &Arc<Self>,
        id: &str,
        run: Option<String>,
    ) -> Result<Option<Origin>> {
        let chat = self.running_chat(id).await?;
        let resumes = match &chat {
            Some(chat) => self.chat_resumes(chat).await?,
            None => false,
        };
        let requested = (self.chat_agents).request(id, chat.as_ref(), resumes, run)?;
        if let Some(chat) = requested.starts {
            // A daemon update waits for the restart: the next daemon would
            // find the chat saved for `shoal resume`, with nothing to bring
            // it back.
            let operation = self.gates.owned_operation().await;
            let manager = self.clone();
            tokio::spawn(async move {
                manager.run_chat_restart(chat).await;
                drop(operation);
            });
        }
        restarted(requested.answer).await
    }

    /// Wait for the restart of the chat `id` that `of_run` asked for as
    /// [`Self::restart_chat`] does, or answer as it did if it ended; it
    /// restarts the chat for `run` too.
    pub(crate) async fn join_chat_restart(
        &self,
        id: &str,
        of_run: Option<String>,
        run: Option<String>,
    ) -> Result<Option<Origin>> {
        let name = match self.recorded_chat(id).await? {
            Some(chat) => chat.name,
            None => id.to_owned(),
        };
        match self.chat_agents.join(id, &name, &of_run, run) {
            Ok(answer) => restarted(answer).await,
            Err(ended) => ended.map_err(|reason| anyhow!(reason)),
        }
    }

    /// The agents of the running chats, and of those a restart stopped to
    /// start again, as `shoal restart` lists agents.
    pub(crate) async fn running_chat_agents(&self) -> Result<Vec<RunningAgent>> {
        let mut chats = Vec::new();
        for chat in self.chats().await? {
            let resumes = self.listed_chat_resumes(&chat).await;
            chats.push((chat, resumes));
        }
        Ok(self.chat_agents.running(chats))
    }

    /// Whether the agent of `chat`, as listed, resumes its session: only a
    /// running chat restarts, and one whose settings cannot be read is listed
    /// as not restorable rather than failing the listing of every agent.
    async fn listed_chat_resumes(&self, chat: &Chat) -> bool {
        if chat.restore.is_some() {
            return false;
        }
        self.chat_resumes(chat).await.unwrap_or_else(|error| {
            log!("chat {} lists as not restorable: {error:#}", chat.id);
            false
        })
    }

    /// Whether the agent of `chat` continues its session when restored: the
    /// chat reported one, and its AI tool has arguments to resume it, without
    /// which a restore starts a new session.
    async fn chat_resumes(&self, chat: &Chat) -> Result<bool> {
        let Some(session) = &chat.session else {
            return Ok(false);
        };
        let settings = self.chat_settings(chat).await?;
        let launch = settings.launch(&chat.agent);
        Ok(launch.arguments.resume_args(Some(session)).is_some())
    }

    /// Restart `chat` and answer the requests for it with how that ended.
    async fn run_chat_restart(&self, chat: Chat) {
        let outcome = self.save_and_restore(&chat).await;
        let outcome = outcome.map_err(|error| format!("{error:#}"));
        self.chat_agents.finish(&chat.id, &outcome);
    }

    /// Save `chat` and stop its agent, then restore it, returning the place
    /// it runs in again. A chat whose restore fails stays stopped for
    /// `shoal resume`.
    async fn save_and_restore(&self, chat: &Chat) -> Result<Option<Origin>> {
        self.save_chat(&chat.id).await?;
        let restored = resume(&self.paths, chat)
            .await
            .map_err(|error| format!("{error:#}"));
        restored
            .and_then(|ended| ended)
            .map_err(|error| anyhow!("{error}; restore it with shoal resume {}", chat.name))?;
        let restored = self.recorded_chat(&chat.id).await?;
        Ok(restored.and_then(|chat| chat.origin))
    }
}

#[cfg(test)]
mod tests;
