//! Waiting for a space's messages and item activity, and building what a
//! watch or a hook delivers to its agent.
use anyhow::{Result, ensure};

use super::{
    Updates, UpdatesExt,
    framing::{Batch, RESPONSE_LIMIT, own_batch, with_unread_messages},
    items::followed_items,
    poll::{until, wait_for_updates, watch_deadline},
    wake_log::WakeWaitLog,
};
use crate::{
    client::{space_ids::SpaceId, space_names::SpaceSelector},
    daemon::{agent_messages::AgentMessage, links::space::LinkSpace, workspace::Manager},
    forge::{
        items::link::{LinkKind, Selection},
        updates::Update,
    },
    model::{PresentSpace, SpaceRef, Workspace},
};

impl Manager {
    pub async fn wait_items(
        &self,
        selector: &SpaceSelector,
        selection: &Selection,
        seconds: Option<u64>,
    ) -> Result<Updates> {
        if let Some(seconds) = seconds {
            ensure!(
                (1..=3600).contains(&seconds),
                "watch timeout must be between 1 and 3600 seconds"
            );
        }
        ensure!(
            selection.input.is_none() || selection.kind.is_some(),
            "explicit items need a kind"
        );
        let deadline = watch_deadline(seconds);
        let workspace = match self.link_space(selector).await? {
            LinkSpace::Workspace(workspace) => *workspace,
            LinkSpace::Chat { chat, .. } => {
                return self.wait_chat_items(&chat.id, selection, deadline).await;
            }
        };
        let wait = self.item_waits.start(&workspace.id);
        // Subscribe before reading, so a message queued in between still wakes.
        let mut queued = self.signals.watch_agent_messages();
        let mut background = self.signals.watch_item_updates();
        let unread = self
            .unread_agent_messages(&SpaceSelector::id(&workspace.id))
            .await?;
        let mut messages = unread.messages;
        if !unread.wakes {
            // A cancelled poll leaves its deliveries pending for the newer wait.
            tokio::select! {
                updates = wait_for_updates(deadline, &mut background, || {
                    self.poll_item_activity(&workspace.id, selection)
                }) => {
                    let updates = updates?;
                    // Including settled ones queued while the wait ran. A
                    // newer delivery may take them and the updates meanwhile.
                    let unread = self.deliverable_messages(&workspace).await?;
                    if !wait.is_current() {
                        return Ok(Updates::superseded());
                    }
                    return with_unread_messages(updates, unread);
                }
                next = self.next_agent_messages(&workspace.id, &mut queued) => messages = next?,
                () = wait.superseded.notified() => return Ok(Updates::superseded()),
            }
        }
        // Messages still settling at the deadline wait for a later delivery.
        let messages = tokio::select! {
            settled = self.settled_messages(&workspace, messages) => settled?,
            () = wait.superseded.notified() => return Ok(Updates::superseded()),
            () = until(deadline) => return Ok(Updates::timed_out()),
        };
        // A newer wait or hook delivery reads the same unacknowledged messages.
        if !wait.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(Updates::messages(messages))
    }

    /// [`Self::agent_updates`] of the workspace, or [`Self::chat_updates`] of
    /// the running chat, `selector` names.
    pub async fn space_updates(&self, selector: &SpaceSelector) -> Result<Updates> {
        match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => {
                self.agent_updates(&SpaceSelector::id(&workspace.id)).await
            }
            PresentSpace::Chat(chat) => self.chat_updates(&chat.id).await,
        }
    }

    /// The next messages or item activity of the agent of the workspace or
    /// running chat `selector` names, unless a newer wait or delivery takes
    /// them first: a workspace's as an unfiltered [`Self::wait_items`]
    /// without a deadline finds them, a chat's as
    /// [`Self::wait_chat_messages`] does. The wake hook's wait, so the daemon
    /// logs how one ends without updates.
    pub async fn wait_space_updates(&self, selector: &SpaceSelector) -> Result<Updates> {
        let space = self.space(selector).await?;
        let log = WakeWaitLog::start(space.name());
        let updates = match space {
            PresentSpace::Workspace(workspace) => {
                (self.wait_items(
                    &SpaceSelector::id(&workspace.id),
                    &Selection::default(),
                    None,
                ))
                .await
            }
            PresentSpace::Chat(chat) => self.wait_chat_messages(&chat.id).await,
        };
        log.answered(&updates);
        updates
    }

    /// What the workspace's agent has not received: its settled messages and
    /// the updates queued on its cursor, without waiting or polling the forge.
    /// Delivery replaces a running watch, which would otherwise deliver the
    /// same updates again. Cleanup and item changes hold a workspace's PR
    /// gate across its forge requests and its removal, longer than an agent
    /// waits for its hook, so while one runs the updates wait for a later
    /// delivery.
    pub async fn agent_updates(&self, selector: &SpaceSelector) -> Result<Updates> {
        let workspace = self.workspace(selector).await?;
        let workspace_id = workspace.id.clone();
        let delivery = self.item_waits.start(&workspace.id);
        let messages = self.deliverable_messages(&workspace).await?;
        // The messages, then the leftovers, share the response with the
        // updates.
        let messages_size = serde_json::to_vec(&messages)?.len();
        let leftovers = self
            .leftovers
            .undelivered(&workspace_id, RESPONSE_LIMIT.saturating_sub(messages_size))?;
        let reserved = messages_size + serde_json::to_vec(&leftovers)?.len();
        let children = self.delegated_spaces(&workspace.id).await?;
        let updates = match self.gates.try_lock_prs(receiving(&workspace.id, &children)) {
            Some(_gate) => {
                self.gated_agent_updates(workspace, &children, reserved)
                    .await?
            }
            None => Vec::new(),
        };
        if !delivery.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(Updates {
            messages,
            updates,
            leftovers,
            timed_out: false,
            superseded: false,
        })
    }

    /// The updates queued for the workspace's agent that fit one response
    /// beside `reserved` bytes of its other content: those on its own cursor
    /// for an unfiltered watch, then those of the spaces below it it works
    /// on. The caller holds the PR gates of the workspace and its `children`.
    pub(super) async fn gated_agent_updates(
        &self,
        workspace: Workspace,
        children: &[SpaceRef],
        reserved: usize,
    ) -> Result<Vec<Update>> {
        let id = workspace.id.clone();
        let mut batch = own_batch(&id, self.queued_updates(workspace).await?);
        batch.extend(self.delegated_pending(children, None).await?);
        self.fit_response(batch, reserved).await
    }

    /// The updates queued on the cursors of `children`, spaces below whose
    /// activity an agent receives, for their items of `kind`, or every kind
    /// and their branches, each naming its space.
    pub(super) async fn delegated_pending(
        &self,
        children: &[SpaceRef],
        kind: Option<LinkKind>,
    ) -> Result<Batch> {
        let mut updates = Vec::new();
        for child in children {
            let child_id = child.id.clone();
            let keys = self
                .store
                .run(move |db| followed_items(db, &child_id, kind))
                .await?
                .into_iter()
                .map(|(url, _)| url)
                .chain(child.branch.clone().filter(|_| kind.is_none()))
                .collect::<Vec<_>>();
            let pending = self.pending_activity(&child.id, &keys).await?;
            updates.extend(pending.into_iter().map(|mut update| {
                update.workspace = Some(child.name.clone());
                (child.id.clone(), update)
            }));
        }
        Ok(updates)
    }

    /// The updates queued on the workspace's cursor for an unfiltered watch.
    pub(super) async fn queued_updates(&self, workspace: Workspace) -> Result<Vec<Update>> {
        let id = workspace.id.clone();
        let items = self
            .store
            .run(move |db| followed_items(db, &id, None))
            .await?;
        let keys = items
            .into_iter()
            .map(|(url, _)| url)
            .chain(workspace.branch)
            .collect::<Vec<_>>();
        self.pending_activity(&workspace.id, &keys).await
    }

    /// [`Self::wait_items`] for the chat `id`: the activity of its items, or
    /// its next messages, which arrive as the user's messages to a chat do,
    /// without settling. It shares its delivery with the chat's hooks, so
    /// the newest of them receives what is pending.
    async fn wait_chat_items(
        &self,
        id: &SpaceId,
        selection: &Selection,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Updates> {
        // The chat's hooks deliver what a watch would, so the newest of them
        // takes it.
        let wait = self.chat_waits.start(id);
        // Subscribe before reading, so a message queued in between still wakes.
        let mut queued = self.signals.watch_chat_messages();
        let mut background = self.signals.watch_item_updates();
        let messages = self.chat_messages(id).await?;
        let updates = if messages.is_empty() {
            tokio::select! {
                updates = wait_for_updates(deadline, &mut background, || {
                    self.poll_item_activity(id, selection)
                }) => updates?,
                messages = self.next_chat_messages(id, &mut queued) => {
                    Updates::messages(messages?)
                }
                () = wait.superseded.notified() => return Ok(Updates::superseded()),
            }
        } else {
            Updates::messages(messages)
        };
        // A newer wait or hook delivery reads the same unacknowledged items.
        if !wait.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(updates)
    }

    /// The chat's messages once it has any.
    async fn next_chat_messages(
        &self,
        id: &SpaceId,
        queued: &mut tokio::sync::watch::Receiver<u64>,
    ) -> Result<Vec<AgentMessage>> {
        loop {
            queued.changed().await?;
            let messages = self.chat_messages(id).await?;
            if !messages.is_empty() {
                return Ok(messages);
            }
        }
    }
    /// The unread messages once one that wakes an idle agent is queued.
    pub(super) async fn next_agent_messages(
        &self,
        id: &SpaceId,
        queued: &mut tokio::sync::watch::Receiver<u64>,
    ) -> Result<Vec<AgentMessage>> {
        loop {
            queued.changed().await?;
            let unread = self.unread_agent_messages(&SpaceSelector::id(id)).await?;
            if unread.wakes {
                return Ok(unread.messages);
            }
        }
    }

    /// The updates queued on the chat's cursor, then those of the spaces
    /// below it whose activity it receives, that fit one response beside
    /// `reserved` bytes of its other content, or `None` while an item change
    /// holds one of their PR gates, when they wait for a later delivery.
    pub(crate) async fn queued_chat_updates(
        &self,
        chat: &SpaceId,
        reserved: usize,
    ) -> Result<Option<Vec<Update>>> {
        let children = self.delegated_spaces(chat).await?;
        let Some(_gate) = self.gates.try_lock_prs(receiving(chat, &children)) else {
            return Ok(None);
        };
        let id = chat.to_owned();
        let keys: Vec<String> = (self.store.run(move |db| followed_items(db, &id, None)))
            .await?
            .into_iter()
            .map(|(url, _)| url)
            .collect();
        let mut batch = own_batch(chat, self.pending_activity(chat, &keys).await?);
        batch.extend(self.delegated_pending(&children, None).await?);
        Ok(Some(self.fit_response(batch, reserved).await?))
    }
}

/// The IDs of space `id` and of `children`, whose activity its agent
/// receives: the PR gates a delivery to that agent holds.
pub(super) fn receiving<'a>(
    id: &'a SpaceId,
    children: &'a [SpaceRef],
) -> impl Iterator<Item = &'a SpaceId> {
    children.iter().map(|child| &child.id).chain([id])
}
