//! The chat a launch records before its agent runs anywhere: below where it
//! goes, with the launch whose slot its agent takes attached, and linked to
//! what it starts on, its starter told what that warned of.
use anyhow::{Context as _, Result};

use crate::{
    daemon::{chats::NewChat, links::issue::AssignerOf, scope::Caller, workspace::Manager},
    forge::items::reference::ItemInput,
    model::{Assigner, Chat, Relation, SpaceLinks},
    protocol::ChatStart,
};
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::chats::LaunchedChat;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Record the chat `start` describes as `new` for `caller`, attach the
    /// launch `entry` to it once it is linked to what it starts on, so the
    /// queue starts its agent in no chat linked only in part, and tell its
    /// starter what linking warned of. A chat its launch cannot take stays
    /// without an agent, for its starter to remove.
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
        if let Some(entry) = entry {
            let attached = self.queue_attach(entry, &SpaceSelector::id(&chat.id)).await;
            attached.with_context(|| {
                let number = chat.number;
                format!("chat {number} stays without its agent, which `shoal rm {number}` removes")
            })?;
        }
        self.tell_start_warnings(&chat.clone().into(), &warnings)
            .await;
        let linked = (self.chat_by_id(&chat.id).await?).context("the new chat was removed")?;
        let chat = Chat {
            links: linked.links,
            ..chat
        };
        Ok(LaunchedChat { chat, warnings })
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
    /// it starts on, assigned, unless its links hold it, and its links as a restore links them, its
    /// PRs as assigned. Returns what that warns of; a start that names
    /// nothing links nothing.
    async fn link_launch(
        &self,
        id: &SpaceId,
        start: &ChatStart,
        caller: Option<&Caller>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(issue) =
            (start.issue.as_ref()).filter(|issue| !links_issue(&start.links, issue))
        {
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
}

/// Whether `links` hold the issue `input` names by its URL.
fn links_issue(links: &SpaceLinks, input: &ItemInput) -> bool {
    let ItemInput::Url(url) = input else {
        return false;
    };
    links.issues.iter().any(|linked| linked.url == *url)
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
