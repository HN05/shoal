//! Workspace-owned activity cursors; waiting does not change completion policy.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, TryStreamExt, stream};
use rusqlite::{OptionalExtension, params};
use tokio::sync::Notify;

use self::cursor::{all_pending, cursor_record, pending_updates, save_cursor_record, save_pending};
use super::{
    pr::RegistrationKind,
    space::{LinkSpace, require_ready_space},
};
use crate::{
    daemon::{agent_messages::AgentMessage, notifications::NotificationKind, workspace::Manager},
    forge::{
        account::Account,
        items::link::{ItemKind, LinkKind, Selection},
        transport::limits::rate_limited,
        updates::{Edited, MergeCheck, Snapshot, Update, UpdateKind},
    },
    model::{ConflictCheck, PresentSpace, SpaceRef, Workspace},
    repository,
};

pub(crate) use crate::client::links::Updates;

mod cursor;

const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// How many spaces the background poll reads at once, and how many of one
/// space's items: each read runs curl processes, and a slow forge must not
/// hold back every other space's poll.
const POLLED_SPACES_AT_ONCE: usize = 4;
const ITEMS_READ_AT_ONCE: usize = 4;

/// The bytes a response's updates and messages may take, leaving room for
/// its other fields.
pub(crate) const RESPONSE_LIMIT: usize = crate::protocol::MAX_FRAME - 256;

/// Conflicted paths named in a branch conflict message; `shoal conflicts` lists all.
const MAX_LISTED_FILES: usize = 10;

const NO_LINKED_ITEMS: &str = "no linked items of the selected kind; register one with shoal link";

/// The running wait of each workspace or chat, identified by its
/// supersession signal. Waits share one cursor per item, so a newer wait
/// supersedes the older one instead of competing for its updates.
#[derive(Default, Clone)]
pub(crate) struct ActiveWaits(Arc<Mutex<HashMap<String, Arc<Notify>>>>);

pub(crate) struct ActiveWait<'a> {
    waits: &'a ActiveWaits,
    workspace: String,
    pub(crate) superseded: Arc<Notify>,
}

impl ActiveWaits {
    pub(crate) fn start(&self, workspace: &str) -> ActiveWait<'_> {
        let superseded = Arc::new(Notify::new());
        let older = self
            .0
            .lock()
            .expect("active waits lock")
            .insert(workspace.to_owned(), superseded.clone());
        if let Some(older) = older {
            older.notify_one();
        }
        ActiveWait {
            waits: self,
            workspace: workspace.to_owned(),
            superseded,
        }
    }
}

impl ActiveWaits {
    /// Run `operation` with the space at the end of each of `paths` that no
    /// wait of `registries`, the workspaces' then the chats', runs in
    /// anywhere along its path, since a wait there receives its updates too,
    /// whatever the kind of the space it runs in; no wait can start in
    /// either while it runs. Every caller locks them in that order.
    fn with_unwatched(
        [workspaces, chats]: [&Self; 2],
        paths: &[Vec<String>],
        operation: impl FnOnce(&[String]) -> Result<()>,
    ) -> Result<()> {
        let workspaces = workspaces.0.lock().expect("active waits lock");
        let chats = chats.0.lock().expect("active waits lock");
        let waiting = |space: &String| workspaces.contains_key(space) || chats.contains_key(space);
        let unwatched = paths
            .iter()
            .filter(|path| !path.iter().any(waiting))
            .filter_map(|path| path.last().cloned())
            .collect::<Vec<_>>();
        operation(&unwatched)
    }
}

#[cfg(test)]
impl ActiveWaits {
    pub(crate) fn is_waiting(&self, key: &str) -> bool {
        self.0.lock().expect("active waits lock").contains_key(key)
    }
}

impl ActiveWait<'_> {
    fn is_current_in(&self, waits: &HashMap<String, Arc<Notify>>) -> bool {
        waits
            .get(&self.workspace)
            .is_some_and(|current| Arc::ptr_eq(current, &self.superseded))
    }

    pub(crate) fn is_current(&self) -> bool {
        self.is_current_in(&self.waits.0.lock().expect("active waits lock"))
    }
}

impl Drop for ActiveWait<'_> {
    fn drop(&mut self) {
        let mut waits = self.waits.0.lock().expect("active waits lock");
        if self.is_current_in(&waits) {
            waits.remove(&self.workspace);
        }
    }
}

impl Manager {
    #[cfg(test)]
    pub async fn wait_prs(&self, selector: &str, seconds: Option<u64>) -> Result<Updates> {
        self.wait_items(
            selector,
            &Selection {
                kind: Some(LinkKind::Pr),
                input: None,
            },
            seconds,
        )
        .await
    }

    pub async fn wait_items(
        &self,
        selector: &str,
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
        let unread = self.unread_agent_messages(&workspace.id).await?;
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
    pub async fn space_updates(&self, selector: &str) -> Result<Updates> {
        match self.present_space(selector).await? {
            PresentSpace::Workspace(workspace) => self.agent_updates(&workspace.id).await,
            PresentSpace::Chat(chat) => self.chat_updates(&chat.id).await,
        }
    }

    /// The next messages or item activity of the agent of the workspace or
    /// running chat `selector` names, unless a newer wait or delivery takes
    /// them first: a workspace's as an unfiltered [`Self::wait_items`]
    /// without a deadline finds them, a chat's as
    /// [`Self::wait_chat_messages`] does.
    pub async fn wait_space_updates(&self, selector: &str) -> Result<Updates> {
        match self.present_space(selector).await? {
            PresentSpace::Workspace(workspace) => {
                (self.wait_items(&workspace.id, &Selection::default(), None)).await
            }
            PresentSpace::Chat(chat) => self.wait_chat_messages(&chat.id).await,
        }
    }

    /// What the workspace's agent has not received: its settled messages and
    /// the updates queued on its cursor, without waiting or polling the forge.
    /// Delivery replaces a running watch, which would otherwise deliver the
    /// same updates again. Cleanup and item changes hold a workspace's PR
    /// gate across its forge requests and its removal, longer than an agent
    /// waits for its hook, so while one runs the updates wait for a later
    /// delivery.
    pub async fn agent_updates(&self, selector: &str) -> Result<Updates> {
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
    async fn gated_agent_updates(
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

    /// The leading updates of `batch` that fit one response beside `reserved`
    /// bytes of its other content. Updates sharing
    /// a delivery are acknowledged together, so a delivery is cut only when
    /// it comes first and does not fit alone; its remainder then moves to a
    /// new delivery and stays pending once the sent part is acknowledged. The
    /// caller holds the PR gates of the workspaces in `batch`.
    async fn fit_response(&self, mut batch: Batch, reserved: usize) -> Result<Vec<Update>> {
        let limit = RESPONSE_LIMIT.saturating_sub(reserved);
        match frame_cut(&batch, limit) {
            Cut::Between(fitting) => batch.truncate(fitting),
            Cut::Inside(fitting) => {
                let (workspace, update) = &batch[0];
                let (workspace, delivery) = (workspace.clone(), update.delivery.clone());
                self.store
                    .run(move |db| redeliver_after(db, &workspace, &delivery, fitting))
                    .await?;
                batch.truncate(fitting);
            }
        }
        Ok(batch.into_iter().map(|(_, update)| update).collect())
    }

    /// The updates queued on the cursors of `children`, spaces below whose
    /// activity an agent receives, for their items of `kind`, or every kind
    /// and their branches, each naming its space.
    async fn delegated_pending(
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
    async fn queued_updates(&self, workspace: Workspace) -> Result<Vec<Update>> {
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

    /// Return once whether the workspace's agent is idle with settled
    /// undelivered messages that wake it or queued updates matches `due`, so a launcher
    /// can prompt an agent that has no hook to wake it, then wait for it to
    /// take them. With `interrupted`, the agent is due only while an
    /// interrupt ended the last turn of that execution's agent, whose hook
    /// wakes it otherwise.
    pub async fn wait_agent_wake(
        &self,
        selector: &str,
        due: bool,
        interrupted: Option<&str>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        // Subscribe before reading, so a change in between still wakes.
        let mut messages = self.signals.watch_agent_messages();
        let mut queued = self.signals.watch_item_updates();
        let mut changed = self.signals.watch_agents();
        loop {
            // One reading decides both whether messages are due and when
            // held ones settle, which no signal announces.
            let unread = self.unread_agent_messages(&workspace.id).await?;
            let hold = self.message_hold(&workspace, &unread.messages).await?;
            let messages_due = wake_messages_due(unread.wakes, hold, due);
            if self
                .agent_wake_due(&workspace, interrupted, messages_due, due)
                .await?
                == due
            {
                return Ok(());
            }
            // Closed channels only disable their wake; the poll interval
            // still rechecks.
            tokio::select! {
                Ok(()) = messages.changed() => {}
                Ok(()) = queued.changed() => {}
                Ok(()) = changed.changed() => {}
                () = self.settle_clock.wait_out(hold) => {}
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    }

    /// Whether messages that wake an idle agent or queued updates wait for
    /// the workspace's agent. While cleanup or an item change holds the PR
    /// gates of the workspaces whose activity it receives, only messages
    /// count, as in a hook delivery.
    pub(crate) async fn updates_waiting(&self, selector: &str) -> Result<bool> {
        let workspace = self.workspace(selector).await?;
        if self.unread_agent_messages(&workspace.id).await?.wakes {
            return Ok(true);
        }
        self.updates_deliverable(workspace).await
    }

    /// Whether updates a hook would deliver are queued for the workspace's
    /// agent. None count while cleanup or an item change holds the PR gates
    /// of the workspaces whose activity it receives, rather than waiting for
    /// them for minutes.
    async fn updates_deliverable(&self, workspace: Workspace) -> Result<bool> {
        let children = self.delegated_spaces(&workspace.id).await?;
        let Some(_gate) = self.gates.try_lock_prs(receiving(&workspace.id, &children)) else {
            return Ok(false);
        };
        Ok(!self
            .gated_agent_updates(workspace, &children, 0)
            .await?
            .is_empty())
    }

    /// Whether messages that wake an idle agent or queued updates are
    /// recorded for the workspace's agent, read without the PR gates, so a
    /// caller holding them, as cleanup does, sees every update their holders
    /// queued.
    pub(crate) async fn agent_updates_recorded(&self, workspace_id: &str) -> Result<bool> {
        let workspace = self.workspace(workspace_id).await?;
        if self.unread_agent_messages(&workspace.id).await?.wakes {
            return Ok(true);
        }
        self.updates_recorded(workspace).await
    }

    /// Whether updates are queued for the workspace's agent, read without
    /// the PR gates.
    async fn updates_recorded(&self, workspace: Workspace) -> Result<bool> {
        let children = self.delegated_spaces(&workspace.id).await?;
        Ok(!self.queued_updates(workspace).await?.is_empty()
            || !self.delegated_pending(&children, None).await?.is_empty())
    }

    /// Whether updates make a wake due for the workspace's agent. Only
    /// `arming` a wake waits for them to be deliverable: once prompted, the
    /// agent stays due while they are recorded, so a gate cleanup takes
    /// meanwhile cannot end that wait and prompt it again.
    async fn wake_updates_due(&self, workspace: Workspace, arming: bool) -> Result<bool> {
        if arming {
            self.updates_deliverable(workspace).await
        } else {
            self.updates_recorded(workspace).await
        }
    }

    /// Whether the workspace's idle agent is due a wake: `messages_due`, or
    /// updates as [`Self::wake_updates_due`] counts them when `arming`.
    async fn agent_wake_due(
        &self,
        workspace: &Workspace,
        interrupted: Option<&str>,
        messages_due: bool,
        arming: bool,
    ) -> Result<bool> {
        if let Some(execution) = interrupted
            && !self.turn_interrupted(execution).await
        {
            return Ok(false);
        }
        let id = workspace.id.clone();
        let state = self
            .store
            .run(move |db| crate::daemon::workspace::agent_state::get(db, &id))
            .await?;
        if state.is_none_or(|status| status.state != crate::state::AgentState::Idle) {
            return Ok(false);
        }
        Ok(messages_due || self.wake_updates_due(workspace.clone(), arming).await?)
    }

    /// The unread messages once one that wakes an idle agent is queued.
    /// [`Self::wait_items`] for the chat `id`: the activity of its items, or
    /// its next messages, which arrive as the user's messages to a chat do,
    /// without settling. It shares its delivery with the chat's hooks, so
    /// the newest of them receives what is pending.
    async fn wait_chat_items(
        &self,
        id: &str,
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
        id: &str,
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

    async fn next_agent_messages(
        &self,
        id: &str,
        queued: &mut tokio::sync::watch::Receiver<u64>,
    ) -> Result<Vec<AgentMessage>> {
        loop {
            queued.changed().await?;
            let unread = self.unread_agent_messages(id).await?;
            if unread.wakes {
                return Ok(unread.messages);
            }
        }
    }

    async fn poll_item_activity(&self, id: &str, selection: &Selection) -> Result<Vec<Update>> {
        // A selected item is the workspace's own.
        let children = match selection.input {
            Some(_) => Vec::new(),
            None => self.delegated_spaces(id).await?,
        };
        let guard = self.gates.lock_prs(receiving(id, &children)).await;
        let space = self.link_space(id).await?;
        let items = self.watched_items(&space, selection).await?;
        let report = branch_report(&items);
        let branch = watches_branch(selection).then(|| space.branch().map(str::to_owned));
        let branch = branch.flatten();
        let mut keys = items.iter().map(|(url, _)| url.clone()).collect::<Vec<_>>();
        keys.extend(branch.clone());
        let mut batch = own_batch(id, self.pending_activity(id, &keys).await?);
        batch.extend(self.delegated_pending(&children, selection.kind).await?);
        // A wait returns messages on their own, without updates.
        let mut pending = self.fit_response(batch, 0).await?;
        if !pending.is_empty() {
            drop(guard);
            self.summarize_failed_checks(id, &mut pending).await;
            return Ok(pending);
        }
        drop(guard);
        let observed_at = crate::time::unix_seconds();
        let items = self.items_to_look_up(&space, items).await?;
        let items = self.item_activity(&space, items, selection).await;
        let mut observations = Vec::new();
        if let Some(branch) = branch {
            observations.extend(self.branch_observation(id, branch, report).await);
        }
        match items {
            Ok(items) => {
                observations.splice(0..0, items);
                let mut updates = self
                    .deliver_observed(id, observations, selection, report, observed_at)
                    .await?;
                self.summarize_failed_checks(id, &mut updates).await;
                Ok(updates)
            }
            // A failed item lookup must not hold back the branch's conflicts.
            Err(error) => {
                let updates = self
                    .deliver_observed(id, observations, selection, report, observed_at)
                    .await?;
                if updates.is_empty() {
                    self.notify_rate_limit(&error).await;
                    return Err(error);
                }
                Ok(updates)
            }
        }
    }

    /// Record `observations` and return the updates that fit one response;
    /// the rest stay pending for the next.
    async fn deliver_observed(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
        selection: &Selection,
        report: BranchReport,
        observed_at: u64,
    ) -> Result<Vec<Update>> {
        let updates = self
            .record_observed(id, observations, selection, report, observed_at)
            .await?;
        self.fit_response(own_batch(id, updates), 0).await
    }

    /// The local check of the workspace `id`'s `branch` as `report` keeps
    /// it, or none when it could report nothing: a linked PR leaves conflicts
    /// to the forge, and the repository lists no sensitive files.
    async fn branch_observation(
        &self,
        id: &str,
        branch: String,
        report: BranchReport,
    ) -> Option<(String, Snapshot)> {
        let only_sensitive = report == BranchReport::ChangedOnBothSides;
        let check = self.watched_conflicts(id, only_sensitive).await;
        Some((branch, branch_conflicts(check.transpose()?, report)))
    }

    /// The `items` whose activity is still to come: a merged PR has none,
    /// and once the space's assignment is done neither has an item seen
    /// closed, so a forge that cannot be reached fails no poll for them.
    async fn items_to_look_up(
        &self,
        space: &LinkSpace,
        items: Vec<(String, LinkKind)>,
    ) -> Result<Vec<(String, LinkKind)>> {
        let (id, done) = (space.id().to_owned(), space.done());
        self.store
            .run(move |db| {
                let mut open = Vec::new();
                for (url, kind) in items {
                    if !activity_ended(db, &id, &url, done)? {
                        open.push((url, kind));
                    }
                }
                Ok(open)
            })
            .await
    }

    /// The activity of `items`, each read from the forge its URL names, so
    /// a worktree Git cannot read holds none of it back.
    async fn item_activity(
        &self,
        space: &LinkSpace,
        items: Vec<(String, LinkKind)>,
        selection: &Selection,
    ) -> Result<Vec<(String, Snapshot)>> {
        let account = &self.user_account();
        let branch = selection.input.is_none().then(|| space.branch());
        let branch = branch.flatten();
        stream::iter(items)
            .map(|(url, kind)| async move {
                let snapshot = self.read_item_activity(account, &url, kind, branch).await?;
                Ok((url, snapshot))
            })
            .buffered(ITEMS_READ_AT_ONCE)
            .try_collect()
            .await
    }

    /// The activity of the item at `url`, a PR only while its head is
    /// `branch` when one is given.
    async fn read_item_activity(
        &self,
        account: &Account,
        url: &str,
        kind: LinkKind,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        let forge = self.item_url_forge(kind, url).await?;
        forge.item_activity(account, kind, url, branch).await
    }

    /// Poll linked items for integrations, with their own baseline, and queue
    /// the changes on each ready workspace's cursor, or hand them to each
    /// running chat's inbox.
    pub(crate) async fn poll_linked_item_activity(&self) -> Result<()> {
        let mut polls = stream::iter(self.polled_spaces().await?)
            .map(|space| async move {
                let result = self.poll_space_item_activity(&space).await;
                (space, result)
            })
            .buffer_unordered(POLLED_SPACES_AT_ONCE);
        let mut failure = None;
        while let Some((space, result)) = polls.next().await {
            let Err(error) = result else {
                continue;
            };
            // The limit is the forge's, not the space's; a later poll retries.
            if self.notify_rate_limit(&error).await {
                continue;
            }
            failure.get_or_insert_with(|| {
                error.context(format!("background item activity for {}", space.name()))
            });
        }
        failure.map_or(Ok(()), Err)
    }

    /// Tell the user when `error` is a forge's rate limit, and whether it is.
    async fn notify_rate_limit(&self, error: &anyhow::Error) -> bool {
        let Some(limit) = rate_limited(error) else {
            return false;
        };
        let until = i64::try_from(limit.until).unwrap_or(i64::MAX);
        let message = format!(
            "{} is limiting Shoal's requests until {}; polling waits until then",
            limit.host,
            crate::time::local_minutes(until)
        );
        self.notify(None, NotificationKind::ForgeLimited, message)
            .await;
        true
    }

    /// The spaces whose linked items the background poll reads: ready
    /// workspaces and running chats.
    async fn polled_spaces(&self) -> Result<Vec<LinkSpace>> {
        let workspaces = self.list_workspaces().await?.into_iter();
        let ready = workspaces.filter(|w| w.state == crate::state::WorkspaceState::Ready);
        let mut spaces: Vec<LinkSpace> = ready.map(LinkSpace::from).collect();
        let chats = self.chats().await?.into_iter();
        for chat in chats.filter(|chat| chat.restore.is_none()) {
            let checkout = self.repository(&chat.repository_id).await?;
            spaces.push(LinkSpace::Chat { chat, checkout });
        }
        Ok(spaces)
    }

    /// Read the activity of the space's polled items, and record and queue
    /// what changed.
    async fn poll_space_item_activity(&self, space: &LinkSpace) -> Result<()> {
        let id = space.id().to_owned();
        let items = self.store.run(move |db| polled_items(db, &id)).await?;
        if items.is_empty() {
            return Ok(());
        }
        let observed_at = crate::time::unix_seconds();
        let report = branch_report(&items);
        let items = self.items_to_look_up(space, items).await?;
        let activity = self
            .item_activity(space, items, &Selection::default())
            .await;
        let mut observations = Vec::new();
        observations.extend(self.background_branch_observation(space, report).await);
        // A failed item lookup must not hold back the branch's changes.
        let looked_up = match activity {
            Ok(activity) => {
                self.record_merges(space, activity.clone()).await?;
                observations.splice(0..0, activity);
                Ok(())
            }
            Err(error) => Err(error),
        };
        self.queue_background_updates(space.id(), observations, observed_at)
            .await?;
        looked_up
    }

    /// The background poll's check of `space`'s branch, made only beside a
    /// linked PR: without one, a watch checks the branch.
    async fn background_branch_observation(
        &self,
        space: &LinkSpace,
        report: BranchReport,
    ) -> Option<(String, Snapshot)> {
        let branch = space
            .branch()
            .filter(|_| report == BranchReport::ChangedOnBothSides)?;
        self.branch_observation(space.id(), branch.to_owned(), report)
            .await
    }

    /// Record the background `activity` of `space`'s items, and sync the
    /// default branch once one of them merged, since other workspaces'
    /// conflict checks read it.
    async fn record_merges(
        &self,
        space: &LinkSpace,
        activity: Vec<(String, Snapshot)>,
    ) -> Result<()> {
        if self
            .record_background_activity(space.id(), activity)
            .await?
        {
            self.syncs.request(space.repository_id());
        }
        Ok(())
    }

    /// The updates queued on the chat's cursor, then those of the spaces
    /// below it whose activity it receives, that fit one response beside
    /// `reserved` bytes of its other content, or `None` while an item change
    /// holds one of their PR gates, when they wait for a later delivery.
    pub(crate) async fn queued_chat_updates(
        &self,
        chat: &str,
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

    /// Queue `observations` on the workspace's cursor, so its agent receives
    /// them without a running watch, and wake the watches that wait for them.
    async fn queue_background_updates(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
        observed_at: u64,
    ) -> Result<()> {
        let all = Selection::default();
        // A background poll checks the branch only beside a linked PR.
        let report = BranchReport::ChangedOnBothSides;
        let updates = self
            .record_observed(id, observations, &all, report, observed_at)
            .await?;
        if !updates.is_empty() {
            self.signals.item_updates_queued();
        }
        Ok(())
    }

    /// Record what changed since the last poll; returns whether an item merged.
    async fn record_background_activity(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
    ) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                require_ready_space(&tx, &id)?;
                let linked = polled_items(&tx, &id)?;
                let mut merged = false;
                for (url, kind, mut snapshot) in
                    observations.into_iter().filter_map(|(url, snapshot)| {
                        linked
                            .iter()
                            .find(|(linked_url, _)| linked_url == &url)
                            .map(|(_, kind)| (url, *kind, snapshot))
                    })
                {
                    let previous: Option<String> = tx
                        .query_row(
                            "SELECT record FROM space_item_activity WHERE space_id=?1 AND url=?2",
                            params![id, url],
                            |row| row.get(0),
                        )
                        .optional()?;
                    let previous: Option<Snapshot> = previous
                        .map(|text| serde_json::from_str(&text))
                        .transpose()?;
                    snapshot.normalize(
                        previous.as_ref().unwrap_or(&Snapshot::default()),
                        crate::time::unix_seconds(),
                    );
                    if let Some(previous) = previous {
                        for update in previous.changes(&snapshot, &url) {
                            merged |= update.kind == UpdateKind::Merged;
                            crate::daemon::events::record_item_activity(
                                &tx, &id, kind, &url, &update,
                            )?;
                        }
                    }
                    tx.execute(
                        "INSERT INTO space_item_activity(space_id,url,record)
                         VALUES (?1,?2,?3)
                         ON CONFLICT(space_id,url) DO UPDATE SET record=excluded.record",
                        params![id, url, serde_json::to_string(&snapshot)?],
                    )?;
                }
                tx.commit()?;
                Ok(merged)
            })
            .await
    }

    /// The canonical URL of an explicitly selected item, linked or not, of
    /// the repository whose origin `checkout` has, or of a CI run of any.
    pub(crate) async fn explicit_item(
        &self,
        checkout: &std::path::Path,
        selection: &Selection,
    ) -> Result<Option<(String, LinkKind)>> {
        let Some(input) = &selection.input else {
            return Ok(None);
        };
        let remote = repository::remote_url_from_path(checkout)
            .await?
            .context("item lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let kind = selection.kind.context("explicit items need a kind")?;
        // A run's URL may name another repository.
        let named = match kind {
            LinkKind::Ci => self.named_run_forge(input).await?,
            LinkKind::Issue | LinkKind::Pr => None,
        };
        let (_, url) = named.unwrap_or(forge).item(kind, input)?;
        Ok(Some((url, kind)))
    }

    /// The explicitly selected item, or the space's linked items of the
    /// selected kind, with every linked run.
    pub(crate) async fn selected_items(
        &self,
        space: &LinkSpace,
        selection: &Selection,
    ) -> Result<Vec<(String, LinkKind)>> {
        let items = self.selection_items(space, selection, shown_items).await?;
        ensure!(
            !items.is_empty() || selection.kind.is_none(),
            "{NO_LINKED_ITEMS}"
        );
        Ok(items)
    }

    /// The space's own items a watch selects. A kind it links none of is
    /// still watched when a child workspace whose activity its agent receives
    /// links one.
    async fn watched_items(
        &self,
        space: &LinkSpace,
        selection: &Selection,
    ) -> Result<Vec<(String, LinkKind)>> {
        let items = self
            .selection_items(space, selection, followed_items)
            .await?;
        if items.is_empty() && selection.input.is_none() {
            let (id, kind) = (space.id().to_owned(), selection.kind);
            let agents = self.tracked_agents().await;
            ensure!(
                self.store
                    .run(move |db| links_kind(db, &id, kind, &agents))
                    .await?,
                "{NO_LINKED_ITEMS}"
            );
        }
        Ok(items)
    }

    /// The explicitly selected item, or the space's linked items of the
    /// selected kind that `linked` lists.
    async fn selection_items(
        &self,
        space: &LinkSpace,
        selection: &Selection,
        linked: LinkedItems,
    ) -> Result<Vec<(String, LinkKind)>> {
        if let Some(item) = self.explicit_item(space.origin_path(), selection).await? {
            return Ok(vec![item]);
        }
        let id = space.id().to_owned();
        let kind = selection.kind;
        self.store.run(move |db| linked(db, &id, kind)).await
    }

    #[cfg(test)]
    async fn record_pr_activity(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
    ) -> Result<Vec<Update>> {
        self.record_activity(
            id,
            observations,
            &Selection {
                kind: Some(LinkKind::Pr),
                input: None,
            },
        )
        .await
    }

    #[cfg(test)]
    async fn record_activity(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
        selection: &Selection,
    ) -> Result<Vec<Update>> {
        let report = BranchReport::Conflicts;
        let now = crate::time::unix_seconds();
        self.record_observed(id, observations, selection, report, now)
            .await
    }

    /// Record `observations` from a poll that started at `observed_at`,
    /// which checked the branch for `report`.
    async fn record_observed(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
        selection: &Selection,
        report: BranchReport,
        observed_at: u64,
    ) -> Result<Vec<Update>> {
        let poll = Poll {
            selection: selection.clone(),
            report,
            observed_at,
        };
        let _guard = self.gates.lock_prs([id]).await;
        let id = id.to_owned();
        let agents = self.tracked_agents().await;
        self.store
            .run(move |db| record_observations(db, &id, observations, &poll, &agents))
            .await
    }

    #[cfg(test)]
    async fn pending_pr_activity(&self, id: &str) -> Result<Vec<Update>> {
        let id_owned = id.to_owned();
        let urls = self
            .store
            .run(move |db| {
                Ok(linked_items(db, &id_owned, Some(ItemKind::Pr))?
                    .into_iter()
                    .map(|(url, _)| url)
                    .collect::<Vec<_>>())
            })
            .await?;
        self.pending_activity(id, &urls).await
    }

    async fn pending_activity(&self, id: &str, urls: &[String]) -> Result<Vec<Update>> {
        let urls = urls.to_vec();
        let id = id.to_owned();
        self.store
            .run(move |db| pending_on_cursor(db, &id, &urls))
            .await
    }

    pub async fn acknowledge_pr_updates(
        &self,
        selector: &str,
        deliveries: Vec<String>,
    ) -> Result<()> {
        let space = self.link_space(selector).await?;
        let id = space.id().to_owned();
        // Without the PR gate: an agent's hook acknowledges within its timeout.
        // One transaction removes exactly the acknowledged deliveries, so
        // whatever a poll or a split queues meanwhile stays pending. A chat's
        // watch shares its delivery with the chat's hooks.
        let waits = [self.item_waits.clone(), self.chat_waits.clone()];
        let agents = self.tracked_agents().await;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                require_ready_space(&tx, &id)?;
                // The agent also received the updates of the spaces below it
                // that it works on.
                let delivered = crate::daemon::workspace::receiving_paths(&tx, &id, &agents)?;
                // A delivering wait has ended, so a wait running in one of
                // these spaces, or in one receiving their activity above them,
                // whatever its kind, started after it and must report their
                // updates as well.
                let [workspaces, chats] = &waits;
                ActiveWaits::with_unwatched([workspaces, chats], &delivered, |unwatched| {
                    acknowledge_deliveries(&tx, unwatched, &deliveries)?;
                    Ok(tx.commit()?)
                })
            })
            .await?;
        self.signals.agent_changed();
        Ok(())
    }
}

/// What a poll whose observations a watch records looked at.
struct Poll {
    selection: Selection,
    /// What the poll's check of the workspace branch reports.
    report: BranchReport,
    /// Unix seconds when the poll started.
    observed_at: u64,
}

/// Record `observations` from `poll` on the cursor of the space `id` in one
/// transaction, and return the updates its watch reports. Observations of
/// items the watch no longer follows are dropped.
fn record_observations(
    db: &mut rusqlite::Connection,
    id: &str,
    observations: Vec<(String, Snapshot)>,
    poll: &Poll,
    agents: &crate::daemon::workspace::TrackedAgents,
) -> Result<Vec<Update>> {
    let observed_at = poll.observed_at;
    let tx = db.transaction()?;
    require_ready_space(&tx, id)?;
    let followed = recorded_urls(&tx, id, poll, agents)?;
    let mut updates = Vec::new();
    for (url, snapshot) in observations {
        if followed
            .as_ref()
            .is_some_and(|followed| !followed.contains(&url))
        {
            continue;
        }
        let mut record = cursor_record(&tx, id, &url)?.unwrap_or_default();
        updates.extend(record.observe(snapshot, &url, observed_at));
        save_cursor_record(&tx, id, &url, &record)?;
    }
    tx.commit()?;
    Ok(updates)
}

/// The URLs whose observations a watch of `poll`'s selection records on the
/// cursor of the space `id`: the items it follows, and the workspace branch
/// when it watches that. `None` for a watch of an explicit item, which
/// records every observation.
fn recorded_urls(
    db: &rusqlite::Connection,
    id: &str,
    poll: &Poll,
    agents: &crate::daemon::workspace::TrackedAgents,
) -> Result<Option<Vec<String>>> {
    let selection = &poll.selection;
    if selection.input.is_some() {
        return Ok(None);
    }
    let items = followed_items(db, id, selection.kind)?;
    if items.is_empty() {
        ensure!(
            links_kind(db, id, selection.kind, agents)?,
            "{NO_LINKED_ITEMS}"
        );
    }
    // A PR linked or unlinked since the poll changes what the branch
    // reports; a chat works on none.
    let branch = if watches_branch(selection) && branch_report(&items) == poll.report {
        workspace_branch(db, id)?
    } else {
        None
    };
    Ok(Some(
        items
            .into_iter()
            .map(|(url, _)| url)
            .chain(branch)
            .collect(),
    ))
}

/// The branch of the workspace `id`; a chat or a workspace at a commit has
/// none.
fn workspace_branch(db: &rusqlite::Connection, id: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT branch FROM workspaces WHERE id=?1", [id], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

/// The IDs of space `id` and of `children`, whose activity its agent
/// receives: the PR gates a delivery to that agent holds.
fn receiving<'a>(id: &'a str, children: &'a [SpaceRef]) -> impl Iterator<Item = &'a str> {
    children.iter().map(|child| child.id.as_str()).chain([id])
}

/// A change the workspace's agent made to `url` through Shoal, which its
/// watches do not report back: the comments, replies or reviews it posted,
/// the state it set and the details it edited.
#[derive(Clone)]
pub(crate) struct OwnChange<'a> {
    pub url: &'a str,
    pub authored: Vec<String>,
    pub state: Option<&'a str>,
    pub edited: Edited,
}

/// Record `change` on the workspace's cursor. The caller holds the workspace's
/// PR gate from before the change, so no poll that observed it can record it
/// first.
pub(crate) fn record_own_change(
    db: &rusqlite::Connection,
    workspace_id: &str,
    change: OwnChange<'_>,
) -> Result<()> {
    let mut record = cursor_record(db, workspace_id, change.url)?.unwrap_or_default();
    record.adopt_own_change(change.authored, change.state, &change.edited);
    save_cursor_record(db, workspace_id, change.url, &record)
}

/// Record `change`, which the space `id` made, on its cursor and on those of
/// the spaces below it whose activity its agent receives and that follow the
/// item, so none of its deliveries reports the change back. A space below
/// with an agent of its own still hears about it. The caller holds the PR
/// gates of these spaces from before the change.
pub(super) fn record_received_change(
    db: &rusqlite::Connection,
    id: &str,
    change: OwnChange<'_>,
    agents: &crate::daemon::workspace::TrackedAgents,
) -> Result<()> {
    for child in crate::daemon::workspace::delegated(db, id, agents)? {
        if follows(db, &child.id, change.url)? {
            record_own_change(db, &child.id, change.clone())?;
        }
    }
    record_own_change(db, id, change)
}

/// Whether the space `id` follows the item at `url`.
fn follows(db: &rusqlite::Connection, id: &str, url: &str) -> Result<bool> {
    Ok(followed_items(db, id, None)?
        .iter()
        .any(|(followed, _)| followed == url))
}

/// Queue an update for the workspace's next watch of `update.url`, ahead of
/// newly observed activity.
pub(super) fn queue_update(
    db: &rusqlite::Connection,
    workspace_id: &str,
    update: Update,
) -> Result<()> {
    let mut pending = pending_updates(db, workspace_id, &update.url)?;
    let url = update.url.clone();
    pending.push(update);
    save_pending(db, workspace_id, &url, &pending)
}

/// Remove `deliveries` from the pending updates of the workspaces `ids`;
/// updates queued after them stay pending.
fn acknowledge_deliveries(
    db: &rusqlite::Connection,
    ids: &[String],
    deliveries: &[String],
) -> Result<()> {
    for (id, url, mut pending) in all_pending(db, ids)? {
        let count = pending.len();
        pending.retain(|update| !deliveries.contains(&update.delivery));
        if pending.len() != count {
            save_pending(db, &id, &url, &pending)?;
        }
    }
    Ok(())
}

/// The updates pending on the cursor of the space `id` for the items at
/// `urls`.
fn pending_on_cursor(db: &rusqlite::Connection, id: &str, urls: &[String]) -> Result<Vec<Update>> {
    Ok(all_pending(db, &[id.to_owned()])?
        .into_iter()
        .filter(|(_, url, _)| urls.contains(url))
        .flat_map(|(_, _, pending)| pending)
        .collect())
}

/// Pending updates for one response, each with the workspace whose cursor
/// holds it.
type Batch = Vec<(String, Update)>;

fn own_batch(id: &str, updates: Vec<Update>) -> Batch {
    updates
        .into_iter()
        .map(|update| (id.to_owned(), update))
        .collect()
}

/// Where a response ends within a batch.
#[derive(Debug, PartialEq, Eq)]
enum Cut {
    /// After this many updates, between deliveries.
    Between(usize),
    /// After this many updates of the first delivery, which does not fit alone.
    Inside(usize),
}

/// Where the updates that fit in `limit` bytes end: after whole deliveries,
/// or inside the first when it alone does not fit.
fn frame_cut(batch: &Batch, limit: usize) -> Cut {
    let sizes = batch
        .iter()
        .map(|(_, update)| serde_json::to_vec(update).map_or(usize::MAX, |bytes| bytes.len()) + 1)
        .collect::<Vec<_>>();
    let mut size = 0_usize;
    let mut fitting = 0;
    while fitting < batch.len() {
        let delivery = &batch[fitting].1.delivery;
        let end = fitting
            + batch[fitting..]
                .iter()
                .take_while(|(_, update)| update.delivery == *delivery)
                .count();
        let group = sizes[fitting..end]
            .iter()
            .fold(0_usize, |size, bytes| size.saturating_add(*bytes));
        if size.saturating_add(group) >= limit {
            if fitting > 0 {
                break;
            }
            let mut size = 0_usize;
            let inside = sizes[..end]
                .iter()
                .take_while(|bytes| {
                    size = size.saturating_add(**bytes);
                    size < limit
                })
                .count();
            return if inside < end {
                Cut::Inside(inside.max(1))
            } else {
                Cut::Between(end)
            };
        }
        size += group;
        fitting = end;
    }
    Cut::Between(fitting)
}

/// Move the pending updates of `delivery` on the workspace's cursor after the
/// first `kept` to a new delivery, so acknowledging `delivery` keeps them.
fn redeliver_after(
    db: &rusqlite::Connection,
    workspace: &str,
    delivery: &str,
    kept: usize,
) -> Result<()> {
    let next = uuid::Uuid::new_v4().to_string();
    let mut seen = 0;
    for (id, url, mut pending) in all_pending(db, &[workspace.to_owned()])? {
        let mut moved = false;
        for update in &mut pending {
            if update.delivery == delivery {
                seen += 1;
                if seen > kept {
                    update.delivery.clone_from(&next);
                    moved = true;
                }
            }
        }
        if moved {
            save_pending(db, &id, &url, &pending)?;
        }
    }
    Ok(())
}

/// What a workspace branch's local check reports: every conflict, or only
/// sensitive files changed on both sides while a linked PR's forge reports
/// its conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchReport {
    Conflicts,
    ChangedOnBothSides,
}

/// What the branch of a space following `items` reports.
fn branch_report(items: &[(String, LinkKind)]) -> BranchReport {
    if items.iter().any(|(_, kind)| *kind == LinkKind::Pr) {
        BranchReport::ChangedOnBothSides
    } else {
        BranchReport::Conflicts
    }
}

/// A workspace branch's local merge check as `report` keeps it, observed
/// like PR mergeability.
fn branch_conflicts(check: Result<ConflictCheck>, report: BranchReport) -> Snapshot {
    Snapshot::merge_check(check.map(|check| MergeCheck {
        conflict: conflict_message(&check, report),
        head: check.head,
        base: check.target_commit,
    }))
}

/// The conflicts `report` keeps of the check, naming the first paths of
/// each kind; `None` when there are none.
fn conflict_message(check: &ConflictCheck, report: BranchReport) -> Option<String> {
    let conflicts = (report == BranchReport::Conflicts && check.conflicts)
        .then(|| format!("Conflicts with {}: {}", check.target, listed(&check.files)));
    let changed = &check.changed_on_both_sides;
    let both = (!changed.is_empty()).then(|| {
        let target = &check.target;
        format!("Changed on both sides with {target}: {}", listed(changed))
    });
    let parts: Vec<String> = conflicts.into_iter().chain(both).collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// The first of `files`, and how many more there are.
fn listed(files: &[String]) -> String {
    let mut listed = files[..files.len().min(MAX_LISTED_FILES)].join(", ");
    if files.len() > MAX_LISTED_FILES {
        listed.push_str(&format!(" and {} more", files.len() - MAX_LISTED_FILES));
    }
    listed
}

/// Whether the workspace, or a child workspace whose activity its agent
/// receives, links an item of `kind`; every workspace has items of no kind.
fn links_kind(
    db: &rusqlite::Connection,
    id: &str,
    kind: Option<LinkKind>,
    agents: &crate::daemon::workspace::TrackedAgents,
) -> Result<bool> {
    if kind.is_none() || !followed_items(db, id, kind)?.is_empty() {
        return Ok(true);
    }
    for child in crate::daemon::workspace::delegated(db, id, agents)? {
        if !followed_items(db, &child.id, kind)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The workspace's items of a kind, or of every kind, that one use of its
/// links covers.
type LinkedItems =
    fn(&rusqlite::Connection, &str, Option<LinkKind>) -> Result<Vec<(String, LinkKind)>>;

/// The items a watch of `kind`, or of every kind, follows: the assigned
/// issue and PRs, and the linked runs whose finish it has yet to deliver.
pub(crate) fn followed_items(
    db: &rusqlite::Connection,
    id: &str,
    kind: Option<LinkKind>,
) -> Result<Vec<(String, LinkKind)>> {
    with_runs(db, id, kind, unfinished_runs)
}

/// The items the background poll reads for integrations: the assigned issue
/// and PRs, and the linked runs its own baseline has not seen finish, apart
/// from what watches delivered.
pub(crate) fn polled_items(db: &rusqlite::Connection, id: &str) -> Result<Vec<(String, LinkKind)>> {
    with_runs(db, id, None, unpolled_runs)
}

/// The items `shoal view` shows for `kind`, or every kind: the assigned
/// issue and PRs, and every linked run.
fn shown_items(
    db: &rusqlite::Connection,
    id: &str,
    kind: Option<LinkKind>,
) -> Result<Vec<(String, LinkKind)>> {
    with_runs(db, id, kind, linked_runs)
}

/// The assigned issue and PRs of `kind`, or of every kind, and the runs
/// `runs` lists when the kind takes runs.
fn with_runs(
    db: &rusqlite::Connection,
    id: &str,
    kind: Option<LinkKind>,
    runs: fn(&rusqlite::Connection, &str) -> Result<Vec<String>>,
) -> Result<Vec<(String, LinkKind)>> {
    let assigned = match kind {
        None => linked_items(db, id, None)?,
        Some(kind) => match kind.item() {
            Some(kind) => linked_items(db, id, Some(kind))?,
            None => Vec::new(),
        },
    };
    let mut items = assigned
        .into_iter()
        .map(|(url, kind)| (url, kind.into()))
        .collect::<Vec<_>>();
    if matches!(kind, None | Some(LinkKind::Ci)) {
        items.extend(runs(db, id)?.into_iter().map(|url| (url, LinkKind::Ci)));
    }
    Ok(items)
}

/// Every run the workspace links, oldest first, once.
fn linked_runs(db: &rusqlite::Connection, id: &str) -> Result<Vec<String>> {
    let urls = db
        .prepare(
            "SELECT url FROM space_items WHERE space_id=?1 AND kind=?2
             ORDER BY created_at, rowid",
        )?
        .query_map(params![id, LinkKind::Ci], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut runs = Vec::new();
    for url in urls {
        if !runs.contains(&url) {
            runs.push(url);
        }
    }
    Ok(runs)
}

/// The workspace's linked runs, except those whose finish the workspace's
/// cursor holds as delivered, which ends their watch.
fn unfinished_runs(db: &rusqlite::Connection, id: &str) -> Result<Vec<String>> {
    let mut runs = linked_runs(db, id)?;
    let mut delivered = Vec::new();
    for url in &runs {
        let record = cursor_record(db, id, url)?;
        if record.is_some_and(|record| record.pending.is_empty() && record.snapshot.run_finished())
        {
            delivered.push(url.clone());
        }
    }
    runs.retain(|url| !delivered.contains(url));
    Ok(runs)
}

/// The workspace's linked runs the background poll has yet to see finish,
/// or to queue the finish of on the workspace's cursor; the two are
/// recorded apart, so a run is polled until both have it.
fn unpolled_runs(db: &rusqlite::Connection, id: &str) -> Result<Vec<String>> {
    let mut runs = linked_runs(db, id)?;
    let mut finished = Vec::new();
    for url in &runs {
        let observed = observed_item(db, id, url)?;
        let queued = cursor_record(db, id, url)?;
        if observed.is_some_and(|snapshot| snapshot.run_finished())
            && queued.is_some_and(|record| record.snapshot.run_finished())
        {
            finished.push(url.clone());
        }
    }
    runs.retain(|url| !finished.contains(url));
    Ok(runs)
}

/// The workspace's assigned issue and PRs, of `kind` or every kind.
pub(crate) fn linked_items(
    db: &rusqlite::Connection,
    id: &str,
    kind: Option<ItemKind>,
) -> Result<Vec<(String, ItemKind)>> {
    let mut items = Vec::new();
    if kind != Some(ItemKind::Issue) {
        let registration: Option<String> = db
            .query_row(
                "SELECT record FROM pr_cleanup WHERE space_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(super::pr::Registration {
            kind: RegistrationKind::Watch { urls, .. },
            ..
        }) = registration
            .map(|text| serde_json::from_str(&text))
            .transpose()?
        {
            items.extend(urls.into_iter().map(|url| (url, ItemKind::Pr)));
        }
    }
    if kind != Some(ItemKind::Pr) {
        let url: Option<String> = db
            .query_row(
                "SELECT url FROM space_issue WHERE space_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        items.extend(url.into_iter().map(|url| (url, ItemKind::Issue)));
    }
    Ok(items)
}

/// Whether the workspace watches a PR that no poll has seen merged or
/// closed, so its activity is still to come.
pub(crate) fn watches_open_pr(db: &rusqlite::Connection, id: &str) -> Result<bool> {
    Ok(!open_prs(db, id)?.is_empty())
}

/// The PRs the workspace watches that no poll has seen merged or closed.
pub(crate) fn open_prs(db: &rusqlite::Connection, id: &str) -> Result<Vec<String>> {
    let mut open = Vec::new();
    for (url, _) in linked_items(db, id, Some(ItemKind::Pr))? {
        if observed_item(db, id, &url)?.is_none_or(|snapshot| !snapshot.concluded()) {
            open.push(url);
        }
    }
    Ok(open)
}

/// The item as the workspace's last poll saw it, if one has.
fn observed_item(db: &rusqlite::Connection, id: &str, url: &str) -> Result<Option<Snapshot>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM space_item_activity WHERE space_id=?1 AND url=?2",
            params![id, url],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

/// Whether the workspace's watch saw the item's activity end, as
/// [`Snapshot::activity_ended`] decides.
fn activity_ended(db: &rusqlite::Connection, id: &str, url: &str, done: bool) -> Result<bool> {
    let record = cursor_record(db, id, url)?;
    Ok(record.is_some_and(|record| record.snapshot.activity_ended(done)))
}

impl Updates {
    pub(crate) fn superseded() -> Self {
        Self {
            updates: Vec::new(),
            messages: Vec::new(),
            leftovers: Vec::new(),
            timed_out: false,
            superseded: true,
        }
    }

    fn timed_out() -> Self {
        Self {
            updates: Vec::new(),
            messages: Vec::new(),
            leftovers: Vec::new(),
            timed_out: true,
            superseded: false,
        }
    }

    pub(crate) fn messages(messages: Vec<AgentMessage>) -> Self {
        Self {
            updates: Vec::new(),
            messages,
            leftovers: Vec::new(),
            timed_out: false,
            superseded: false,
        }
    }
}

/// `updates` with the unread `messages` when the wait found updates and both
/// fit one response; otherwise the messages wait for a later delivery.
fn with_unread_messages(mut updates: Updates, messages: Vec<AgentMessage>) -> Result<Updates> {
    if updates.updates.is_empty() || messages.is_empty() {
        return Ok(updates);
    }
    let size = serde_json::to_vec(&updates)?.len() + serde_json::to_vec(&messages)?.len();
    if size <= RESPONSE_LIMIT {
        updates.messages = messages;
    }
    Ok(updates)
}

/// Whether a wait also reports the workspace branch's local conflicts with
/// its base, as [`branch_report`] decides: only an unfiltered wait.
fn watches_branch(selection: &Selection) -> bool {
    selection.input.is_none() && selection.kind.is_none()
}

/// Whether unread messages, of which one `wakes` the agent and which settle
/// after `hold`, make a wake due. Settling only delays `arming` a wake: once
/// prompted, the agent stays due until it takes them or starts a turn, so
/// a message arriving meanwhile cannot end that wait and prompt it again.
fn wake_messages_due(wakes: bool, hold: Option<Duration>, arming: bool) -> bool {
    wakes && (hold.is_none() || !arming)
}

/// When a watch with a timeout of `seconds` started now ends.
fn watch_deadline(seconds: Option<u64>) -> Option<tokio::time::Instant> {
    seconds.map(|seconds| tokio::time::Instant::now() + Duration::from_secs(seconds))
}

/// Resolves at `deadline`, or never without one.
async fn until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Poll every [`POLL_INTERVAL`], or once a forge's rate limit ends, and at
/// once whenever `queued` changes, until `deadline`.
async fn wait_for_updates<F: std::future::Future<Output = Result<Vec<Update>>>>(
    deadline: Option<tokio::time::Instant>,
    queued: &mut tokio::sync::watch::Receiver<u64>,
    mut poll: impl FnMut() -> F,
) -> Result<Updates> {
    loop {
        // A cancelled poll may already have committed a pending delivery. The
        // next wait replays it, so the advertised timeout can remain strict.
        let polled = match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, poll()).await {
                Ok(polled) => polled,
                Err(_) => return Ok(Updates::timed_out()),
            },
            None => poll().await,
        };
        let (updates, interval) = limited_poll(polled, crate::time::unix_seconds())?;
        if !updates.is_empty() {
            return Ok(Updates {
                updates,
                messages: Vec::new(),
                leftovers: Vec::new(),
                timed_out: false,
                superseded: false,
            });
        }
        let sleep_for = if let Some(deadline) = deadline {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(Updates::timed_out());
            }
            interval.min(deadline - now)
        } else {
            interval
        };
        // A closed channel only disables the early wake.
        tokio::select! {
            () = tokio::time::sleep(sleep_for) => {}
            Ok(()) = queued.changed() => {}
        }
    }
}

/// The updates of a poll and how long to wait before the next one at `now`
/// in Unix seconds: the poll interval, or until a forge that refused the
/// poll accepts requests.
fn limited_poll(polled: Result<Vec<Update>>, now: u64) -> Result<(Vec<Update>, Duration)> {
    match polled {
        Ok(updates) => Ok((updates, POLL_INTERVAL)),
        Err(error) => {
            let Some(limit) = rate_limited(&error) else {
                return Err(error);
            };
            Ok((Vec::new(), POLL_INTERVAL.max(limit.wait_from(now))))
        }
    }
}

#[cfg(test)]
mod tests;
