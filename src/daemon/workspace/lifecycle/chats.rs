//! Removing a chat through the removal path every space takes: the spaces
//! below it go first, whatever keeps them, then its agent and its record.
use anyhow::{Context as _, Result, bail};

use super::{Removal, Retention};
use crate::{
    client::output::chat::RemovedChat,
    daemon::workspace::Manager,
    model::Chat,
    removal::{InspectionPolicy, UnsavedWork},
    state::ChatRestore,
};

impl Manager {
    /// Remove the chat `selector` names, by ID or name, at the user's
    /// request, stopping its agent: first the spaces below it, of which
    /// `children`, when given, are the IDs the user saw and the only ones it
    /// takes. Returns the chat and the names of the spaces it took.
    pub async fn remove_chat(
        &self,
        selector: &str,
        children: Option<Vec<String>>,
    ) -> Result<RemovedChat> {
        let chat = self
            .recorded_chat(selector)
            .await?
            .with_context(|| format!("no chat {selector}"))?;
        let unsaved = UnsavedWork::Confirmed {
            files: false,
            detached_head: None,
            files_restorable: false,
            children,
        };
        let removal = Removal::Manual {
            unsaved: &unsaved,
            inspection: InspectionPolicy::GitOnly,
            release_holds: true,
        };
        let children = self.remove_chat_for(&chat, removal).await?;
        Ok(RemovedChat { chat, children })
    }

    /// Remove `chat` for `removal`: the spaces below it, through the path
    /// that removes them below any space, then, once nothing keeps it from
    /// an unattended removal, its agent and its record. Returns the names
    /// of the spaces below it it took, deepest first.
    pub(super) async fn remove_chat_for(
        &self,
        chat: &Chat,
        removal: Removal<'_>,
    ) -> Result<Vec<String>> {
        self.ensure_chat_unkept(&chat.id, removal).await?;
        let below = self.chat_children(&chat.id).await?;
        self.ensure_none_stalled_below(below.clone(), removal)
            .await?;
        let children = self.remove_children(&chat.name, below, removal).await?;
        // A save or restore under way would run the agent the removal stops.
        let _saving = self.chat_saves.lock().await;
        let chat = self
            .recorded_chat(&chat.id)
            .await?
            .context("the chat was removed meanwhile")?;
        self.ensure_chat_unkept(&chat.id, removal).await?;
        if chat.restore.is_none() {
            // Idle cleanup takes only a chat whose agent stopped.
            if matches!(removal, Removal::Automatic { .. }) {
                bail!("the chat's agent runs again; retaining it");
            }
            self.stop_chat(&chat.id).await?;
        }
        self.ensure_ended(&chat, std::process::id()).await?;
        self.delete_chat(&chat).await?;
        Ok(children)
    }

    /// Refuse an unattended `removal` of the chat `id` while something keeps
    /// it: what keeps any space, or a restore that waits to run its agent.
    async fn ensure_chat_unkept(&self, id: &str, removal: Removal<'_>) -> Result<()> {
        if !removal.unattended() {
            return Ok(());
        }
        let retention = match self.keep_reason(id).await? {
            Some(retention) => Some(retention),
            None => (self.recorded_chat(id).await?)
                .is_some_and(|chat| chat.restore == Some(ChatRestore::Automatic))
                .then_some(Retention::QueuedAgent),
        };
        if let Some(retention) = retention {
            bail!("{retention}; retaining the chat");
        }
        Ok(())
    }
}
