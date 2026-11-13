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
    daemon::{agent_messages::AgentMessage, workspace::Manager},
    forge::{
        items::link::{LinkKind, Selection},
        updates::Update,
    },
    model::{PresentSpace, SpaceRef},
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
        let space = self.space(selector).await?;
        // The space's hooks deliver what a watch would, so the newest of them
        // takes it.
        let wait = self.waits.start(space.id());
        // Subscribe before reading, so a message queued in between still wakes.
        let mut queued = self.signals.watch_messages();
        let mut background = self.signals.watch_item_updates();
        let unread = self.unread_messages(space.id()).await?;
        let mut messages = unread.messages;
        if !unread.wakes {
            // A cancelled poll leaves its deliveries pending for the newer wait.
            tokio::select! {
                updates = wait_for_updates(deadline, &mut background, || {
                    self.poll_space_activity(&space, selection)
                }) => {
                    let updates = updates?;
                    // Including settled ones queued while the wait ran. A
                    // newer delivery may take them and the updates meanwhile.
                    let unread = self.deliverable_messages(&space).await?;
                    if !wait.is_current() {
                        return Ok(Updates::superseded());
                    }
                    return with_unread_messages(updates, unread);
                }
                next = self.next_agent_messages(space.id(), &mut queued) => messages = next?,
                () = wait.superseded.notified() => return Ok(Updates::superseded()),
            }
        }
        // Messages still settling at the deadline wait for a later delivery.
        let messages = tokio::select! {
            settled = self.settled_messages(&space, messages) => settled?,
            () = wait.superseded.notified() => return Ok(Updates::superseded()),
            () = until(deadline) => return Ok(Updates::timed_out()),
        };
        // A newer wait or hook delivery reads the same unacknowledged messages.
        if !wait.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(Updates::messages(messages))
    }

    /// The next messages or item activity of the agent of the space
    /// `selector` names, as an unfiltered [`Self::wait_items`] without a
    /// deadline finds them, unless a newer wait or delivery takes them
    /// first. The wake hook's wait, so the daemon logs how one ends without
    /// updates.
    pub async fn wait_space_updates(&self, selector: &SpaceSelector) -> Result<Updates> {
        let space = self.space(selector).await?;
        let log = WakeWaitLog::start(space.name());
        let selector = SpaceSelector::id(space.id());
        let updates = self
            .wait_items(&selector, &Selection::default(), None)
            .await;
        log.answered(&updates);
        updates
    }

    /// What the agent of the space `selector` names has not received: its
    /// settled messages, its leftovers and the updates queued on its
    /// cursor, without waiting or polling the forge. Delivery replaces a
    /// running watch, which would otherwise deliver the same updates again.
    /// Cleanup and item changes hold a space's PR gate across their forge
    /// requests and its removal, longer than an agent waits for its hook,
    /// so while one runs the updates wait for a later delivery.
    pub async fn agent_updates(&self, selector: &SpaceSelector) -> Result<Updates> {
        let space = self.space(selector).await?;
        let id = space.id().clone();
        let delivery = self.waits.start(&id);
        let messages = self.deliverable_messages(&space).await?;
        // The messages, then the leftovers, share the response with the
        // updates.
        let messages_size = serde_json::to_vec(&messages)?.len();
        let leftovers = self
            .leftovers
            .undelivered(&id, RESPONSE_LIMIT.saturating_sub(messages_size))?;
        let reserved = messages_size + serde_json::to_vec(&leftovers)?.len();
        let children = self.delegated_spaces(&id).await?;
        let updates = match self.gates.try_lock_prs(receiving(&id, &children)) {
            Some(_gate) => {
                self.gated_agent_updates(&space, &children, None, reserved)
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

    /// The updates queued for the agent of `space` that fit one response
    /// beside `reserved` bytes of its other content: those on its own
    /// cursor for its items of `kind`, or every kind and its branch, then
    /// those of the spaces below it it works on. The caller holds the PR
    /// gates of the space and its `children`.
    pub(super) async fn gated_agent_updates(
        &self,
        space: &PresentSpace,
        children: &[SpaceRef],
        kind: Option<LinkKind>,
        reserved: usize,
    ) -> Result<Vec<Update>> {
        let mut batch = own_batch(space.id(), self.queued_updates(space, kind).await?);
        batch.extend(self.delegated_pending(children, kind).await?);
        self.fit_response(batch, reserved).await
    }

    /// The updates of `kind`, or every kind, queued for the agent of
    /// `space` that fit one response, once nothing holds the PR gates of
    /// the space and the spaces below it whose activity it receives.
    pub(super) async fn queued_activity(
        &self,
        space: &PresentSpace,
        kind: Option<LinkKind>,
    ) -> Result<Vec<Update>> {
        let children = self.delegated_spaces(space.id()).await?;
        let _gate = self.gates.lock_prs(receiving(space.id(), &children)).await;
        self.gated_agent_updates(space, &children, kind, 0).await
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
                .map(|url| url.to_string())
                .chain(
                    child
                        .branch
                        .clone()
                        .filter(|_| kind.is_none())
                        .map(String::from),
                )
                .collect::<Vec<_>>();
            let pending = self.pending_activity(&child.id, &keys).await?;
            updates.extend(pending.into_iter().map(|mut update| {
                update.workspace = Some(child.name.to_string());
                (child.id.clone(), update)
            }));
        }
        Ok(updates)
    }

    /// The updates queued on the cursor of `space` for a watch of `kind`:
    /// those of its items of that kind, or of every kind and its branch,
    /// when it has one.
    pub(super) async fn queued_updates(
        &self,
        space: &PresentSpace,
        kind: Option<LinkKind>,
    ) -> Result<Vec<Update>> {
        let id = space.id().clone();
        let items = self
            .store
            .run(move |db| followed_items(db, &id, kind))
            .await?;
        let branch = (space.worktree())
            .filter(|_| kind.is_none())
            .and_then(|workspace| workspace.branch.clone());
        let keys = items
            .into_iter()
            .map(|url| url.to_string())
            .chain(branch.map(String::from))
            .collect::<Vec<_>>();
        self.pending_activity(space.id(), &keys).await
    }

    /// The unread messages once one that wakes an idle agent is queued.
    pub(super) async fn next_agent_messages(
        &self,
        id: &SpaceId,
        queued: &mut tokio::sync::watch::Receiver<u64>,
    ) -> Result<Vec<AgentMessage>> {
        loop {
            queued.changed().await?;
            let unread = self.unread_messages(id).await?;
            if unread.wakes {
                return Ok(unread.messages);
            }
        }
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
