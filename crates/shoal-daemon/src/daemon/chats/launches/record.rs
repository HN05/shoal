//! The chat a launch records before its agent runs anywhere: below where it
//! goes, with the launch whose slot its agent takes attached, and linked to
//! what it starts on, its starter told what that warned of; and the removal
//! of one whose agent never started.
use std::pin::Pin;

use anyhow::{Context as _, Result};

use crate::{
    daemon::{
        chats::NewChat, links::issue::AssignerOf, notifications::NotificationKind, scope::Caller,
        workspace::Manager,
    },
    model::{Assigner, Chat, Relation, SpaceLinks},
    protocol::ChatStart,
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::chats::LaunchedChat;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

impl Manager {
    /// Record the chat `start` describes as `new` for `caller`, attach the
    /// launch `entry` to it once it is linked to what it starts on, so the
    /// queue starts its agent in no chat linked only in part, and tell its
    /// starter what linking warned of. A chat its launch cannot take is
    /// removed again.
    pub(in crate::daemon) async fn record_launch(
        &self,
        start: &ChatStart,
        new: NewChat,
        entry: Option<&QueueEntryId>,
        caller: Option<&Caller>,
    ) -> Result<LaunchedChat> {
        let workspace = start.workspace.as_ref();
        let chat = (self.start_new_chat(start.repository.clone(), workspace, new)).await?;
        let warnings = self.link_launch(&chat.id, start, caller).await;
        if let Some(entry) = entry
            && let Err(error) = self.queue_attach(entry, &SpaceSelector::id(&chat.id)).await
        {
            self.discard_new_chat(&chat.name, &chat.id).await;
            return Err(error);
        }
        self.tell_start_warnings(&chat, &warnings).await;
        let linked = (self.chat_by_id(&chat.id).await?).context("the new chat was removed")?;
        let chat = Chat {
            links: linked.links,
            ..chat
        };
        Ok(LaunchedChat { chat, warnings })
    }

    /// The chat the daemon recorded for a start whose agent its caller
    /// runs: `chat`, else the one the launch `entry` recorded while it
    /// waited.
    pub(in crate::daemon) async fn recorded_launch(
        &self,
        chat: Option<&SpaceSelector>,
        entry: Option<&QueueEntryId>,
    ) -> Result<Option<SpaceId>> {
        if let Some(chat) = self.space_id(chat).await? {
            return Ok(Some(chat));
        }
        let Some(entry) = entry else {
            return Ok(None);
        };
        let entries = self.queue_entries().await?;
        let queued = entries.into_iter().find(|queued| queued.id == *entry);
        Ok(queued.and_then(|queued| queued.space_id))
    }

    /// The chat `id` the daemon recorded for a start whose agent its caller
    /// runs, placed at `origin` when it runs in a terminal host.
    pub(in crate::daemon) async fn take_recorded_chat(
        &self,
        id: &SpaceId,
        origin: Option<crate::sessions::Origin>,
    ) -> Result<LaunchedChat> {
        if let Some(origin) = origin {
            self.place_chat(&SpaceSelector::id(id), origin).await?;
        }
        let chat = self.chat_by_id(id).await?;
        let chat = chat.context("the chat recorded for the start is gone")?;
        let warnings = Vec::new();
        Ok(LaunchedChat { chat, warnings })
    }

    /// Link the new chat `id` for `caller` to what `start` names: the issue
    /// it starts on, assigned, and its links as a restore links them, its
    /// PRs as assigned. Returns what that warns of; a start that names
    /// nothing links nothing.
    async fn link_launch(
        &self,
        id: &SpaceId,
        start: &ChatStart,
        caller: Option<&Caller>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(issue) = &start.issue {
            let selector = SpaceSelector::id(id);
            match (self.set_issue(&selector, issue, None, Assigner::of(caller))).await {
                Ok(assigned) => warnings.extend(assigned),
                Err(error) => warnings.push(format!("cannot link issue {issue}: {error:#}")),
            }
        }
        if !start.links.is_empty() {
            warnings.extend(self.relink_launch(id, &start.links, caller).await);
        }
        warnings
    }

    /// Link `links` to the new chat `id` for `caller` as a restore links
    /// them, its PRs as assigned, returning what that warns of.
    async fn relink_launch(
        &self,
        id: &SpaceId,
        links: &SpaceLinks,
        caller: Option<&Caller>,
    ) -> Vec<String> {
        let mut warnings = self.relink_space(id, links, caller).await;
        for pr in &links.prs {
            let (selector, input) = (SpaceSelector::id(id), pr.url.clone().into());
            let title = pr.title.clone();
            let assigned = Relation::Assigned;
            match (self.link_item(&selector, pr.kind(), &input, assigned, title, caller)).await {
                Ok(linked) => warnings.extend(linked.warnings),
                Err(error) => warnings.push(format!("cannot link {}: {error:#}", pr.url)),
            }
        }
        warnings
    }

    /// Tell the starter of the new `chat` what starting it warned of: the
    /// agent of the nearest space above it with one, else the user.
    async fn tell_start_warnings(&self, chat: &Chat, warnings: &[String]) {
        if warnings.is_empty() {
            return;
        }
        let message = format!("starting {} warned: {}", chat.name, warnings.join("; "));
        let kind = NotificationKind::StartWarned;
        self.notify_space(&chat.name, &chat.id, kind, message).await;
    }

    /// Remove the new chat `id`, named `name`, whose agent did not start
    /// with `error`, telling its starter as its warnings are; one whose
    /// agent runs after all stays.
    pub(in crate::daemon) async fn fail_new_chat(
        &self,
        name: &SpaceName,
        id: &SpaceId,
        error: &str,
    ) {
        match self.chat_by_id(id).await {
            Ok(Some(chat)) if !chat.running => {}
            _ => return,
        }
        let message = format!("{name} did not start, so Shoal removed it: {error}");
        (self.notify_space(name, id, NotificationKind::StartFailed, message)).await;
        self.discard_new_chat(name, id).await;
    }

    /// Remove the new chat `id`, named `name`, which nothing ran in.
    async fn discard_new_chat(&self, name: &SpaceName, id: &SpaceId) {
        let selector = SpaceSelector::id(id);
        let unsaved = UnsavedWork::taking(Vec::new());
        // Boxed, since a removal can end a queue entry whose failure removes
        // its new chat.
        let removed: Pin<Box<dyn Future<Output = Result<_>> + Send + '_>> =
            Box::pin(self.remove_selected(&selector, unsaved, InspectionPolicy::GitOnly));
        if let Err(error) = removed.await {
            crate::daemon::log!("chat {name} kept without its agent: {error:#}");
        }
    }
}

impl NewChat {
    /// The chat `start` describes, which names its name and agent, below
    /// `parent` and running at `origin` when given.
    pub(in crate::daemon) fn of(
        start: &ChatStart,
        parent: Option<SpaceId>,
        origin: Option<crate::sessions::Origin>,
    ) -> Result<Self> {
        Ok(Self {
            name: start.name.clone().context("the start names the chat")?,
            agent: (start.agent.clone()).context(crate::agents::NO_AGENT)?,
            mode: (start.mode.clone()).unwrap_or_else(crate::model::chat_mode),
            narrowing: start.narrowing,
            origin,
            parent,
            priority: start.priority,
        })
    }
}

#[cfg(test)]
mod tests;
