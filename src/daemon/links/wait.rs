//! Workspace-owned activity cursors; waiting does not change completion policy.
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use self::held::HeldPasses;
use super::pr::RegistrationKind;
use crate::{
    daemon::{
        agent_messages::AgentMessage, notifications::NotificationKind, store, workspace::Manager,
    },
    forge::{
        items::{
            item::ItemState,
            link::{ItemKind, LinkKind, Selection},
        },
        transport::limits::rate_limited,
        updates::{MergeCheck, Snapshot, Update},
    },
    model::{ConflictCheck, Workspace, WorkspaceRef},
    repository,
};

mod held;

const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// The bytes a response's updates and messages may take, leaving room for
/// its other fields.
const RESPONSE_LIMIT: usize = crate::protocol::MAX_FRAME - 256;

/// Conflicted paths named in a branch conflict message; `shoal conflicts` lists all.
const MAX_LISTED_FILES: usize = 10;

const NO_LINKED_ITEMS: &str = "no linked items of the selected kind; register one with shoal link";

#[derive(Debug, Serialize, Deserialize)]
pub struct Updates {
    pub updates: Vec<Update>,
    /// Agent messages end a wait on their own, ahead of item activity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<AgentMessage>,
    pub timed_out: bool,
    /// A newer wait in the same workspace took over before this one received
    /// updates; they stay pending for the newer wait.
    #[serde(default)]
    pub superseded: bool,
    /// Whether a terminal shows the agent asking, when that changed since
    /// its hooks last told it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached: Option<bool>,
}

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
    /// Run `operation` with the workspace at the end of each of `paths` that
    /// no wait runs in anywhere along its path, since a wait there receives
    /// its updates too; no wait can start while it runs.
    fn with_unwatched(
        &self,
        paths: &[Vec<String>],
        operation: impl FnOnce(&[String]) -> Result<()>,
    ) -> Result<()> {
        let waits = self.0.lock().expect("active waits lock");
        let unwatched = paths
            .iter()
            .filter(|path| !path.iter().any(|workspace| waits.contains_key(workspace)))
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

#[derive(Default, Serialize, Deserialize)]
struct ActivityRecord {
    #[serde(flatten)]
    snapshot: Snapshot,
    #[serde(default)]
    pending: Vec<Update>,
    /// Comments and reviews the workspace changed through Shoal that no
    /// poll has seen change yet.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    authored: BTreeSet<String>,
    /// Unix seconds when the workspace last set the item's state through
    /// Shoal; observations that started by then cannot show it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    own_state_at: Option<u64>,
    #[serde(default, skip_serializing_if = "HeldPasses::is_empty")]
    held: HeldPasses,
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
        let workspace = self.workspace(selector).await?;
        let wait = self.item_waits.start(&workspace.id);
        // Subscribe before reading, so a message queued in between still wakes.
        let mut queued = self.signals.watch_agent_messages();
        let mut background = self.signals.watch_item_updates();
        let unread = self.unread_agent_messages(&workspace.id).await?;
        let mut messages = unread.messages;
        if !unread.wakes {
            // A cancelled poll leaves its deliveries pending for the newer wait.
            tokio::select! {
                updates = wait_for_updates(seconds, &mut background, || {
                    self.poll_item_activity(&workspace.id, selection)
                }) => {
                    let updates = updates?;
                    // Including those queued while the wait ran. A newer
                    // delivery may take them and the updates meanwhile.
                    let unread = self.agent_messages(&workspace.id).await?;
                    if !wait.is_current() {
                        return Ok(Updates::superseded());
                    }
                    return with_unread_messages(updates, unread);
                }
                next = self.next_agent_messages(&workspace.id, &mut queued) => messages = next?,
                () = wait.superseded.notified() => return Ok(Updates::superseded()),
            }
        }
        // A newer wait or hook delivery reads the same unacknowledged messages.
        if !wait.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(Updates::messages(messages))
    }

    /// What the workspace's agent has not received: its messages and the
    /// updates queued on its cursor, without waiting or polling the forge.
    /// Delivery replaces a running watch, which would otherwise deliver the
    /// same updates again. Cleanup and item changes hold a workspace's PR
    /// gate across its forge requests and its removal, longer than an agent
    /// waits for its hook, so while one runs the updates wait for a later
    /// delivery.
    pub async fn agent_updates(&self, selector: &str) -> Result<Updates> {
        let workspace = self.workspace(selector).await?;
        let delivery = self.item_waits.start(&workspace.id);
        let messages = self.agent_messages(&workspace.id).await?;
        // The messages share the response with the updates.
        let reserved = serde_json::to_vec(&messages)?.len();
        let children = self.delegated_workspaces(&workspace.id).await?;
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
            timed_out: false,
            superseded: false,
            attached: None,
        })
    }

    /// The updates queued for the workspace's agent that fit one response
    /// beside `reserved` bytes of its other content: those on its own cursor
    /// for an unfiltered watch, then those of the child workspaces it works on.
    async fn agent_queued_updates(
        &self,
        workspace: Workspace,
        reserved: usize,
    ) -> Result<Vec<Update>> {
        let children = self.delegated_workspaces(&workspace.id).await?;
        let _gate = self
            .gates
            .lock_prs(receiving(&workspace.id, &children))
            .await;
        self.gated_agent_updates(workspace, &children, reserved)
            .await
    }

    /// [`Self::agent_queued_updates`] for a caller already holding the PR
    /// gates of the workspace and its `children`.
    async fn gated_agent_updates(
        &self,
        workspace: Workspace,
        children: &[WorkspaceRef],
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

    /// The updates queued on the cursors of `children`, child workspaces
    /// whose activity an agent receives, for their items of `kind` or every
    /// kind, each naming its workspace.
    async fn delegated_pending(
        &self,
        children: &[WorkspaceRef],
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
        let branch = watches_branch(&Selection::default(), &items).then_some(workspace.branch);
        let keys = items
            .into_iter()
            .map(|(url, _)| url)
            .chain(branch)
            .collect::<Vec<_>>();
        self.pending_activity(&workspace.id, &keys).await
    }

    /// Return once whether the workspace's agent is idle with undelivered
    /// messages that wake it or queued updates matches `due`, so a launcher
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
        while self.agent_wake_due(&workspace, interrupted).await? != due {
            // Closed channels only disable their wake; the poll interval
            // still rechecks.
            tokio::select! {
                Ok(()) = messages.changed() => {}
                Ok(()) = queued.changed() => {}
                Ok(()) = changed.changed() => {}
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
        Ok(())
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
        let children = self.delegated_workspaces(&workspace.id).await?;
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
        let children = self.delegated_workspaces(&workspace.id).await?;
        Ok(!self.queued_updates(workspace).await?.is_empty()
            || !self.delegated_pending(&children, None).await?.is_empty())
    }

    async fn agent_wake_due(
        &self,
        workspace: &Workspace,
        interrupted: Option<&str>,
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
        Ok(self.unread_agent_messages(&workspace.id).await?.wakes
            || !self
                .agent_queued_updates(workspace.clone(), 0)
                .await?
                .is_empty())
    }

    /// The unread messages once one that wakes an idle agent is queued.
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
            None => self.delegated_workspaces(id).await?,
        };
        let guard = self.gates.lock_prs(receiving(id, &children)).await;
        let workspace = self.workspace(id).await?;
        let items = self.watched_items(&workspace, selection).await?;
        let branch = watches_branch(selection, &items).then(|| workspace.branch.clone());
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
        let items = self.items_to_look_up(&workspace, items).await?;
        let items = self.item_activity(&workspace, items, selection).await;
        let mut observations = Vec::new();
        if let Some(branch) = branch {
            let check = self.watched_conflicts(id).await;
            observations.push((branch, branch_conflicts(check)));
        }
        match items {
            Ok(items) => {
                observations.splice(0..0, items);
                let mut updates = self
                    .deliver_observed(id, observations, selection, observed_at)
                    .await?;
                self.summarize_failed_checks(id, &mut updates).await;
                Ok(updates)
            }
            // A failed item lookup must not hold back the branch's conflicts.
            Err(error) => {
                let updates = self
                    .deliver_observed(id, observations, selection, observed_at)
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
        observed_at: u64,
    ) -> Result<Vec<Update>> {
        let updates = self
            .record_observed(id, observations, selection, observed_at)
            .await?;
        self.fit_response(own_batch(id, updates), 0).await
    }

    /// The `items` whose activity is still to come: a merged PR has none,
    /// and once the workspace's assignment is done neither has an item seen
    /// closed, so a forge that cannot be reached fails no poll for them.
    async fn items_to_look_up(
        &self,
        workspace: &Workspace,
        items: Vec<(String, LinkKind)>,
    ) -> Result<Vec<(String, LinkKind)>> {
        let (id, done) = (workspace.id.clone(), workspace.done);
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
        workspace: &Workspace,
        items: Vec<(String, LinkKind)>,
        selection: &Selection,
    ) -> Result<Vec<(String, Snapshot)>> {
        let account = self.user_account();
        let mut observations = Vec::new();
        for (url, kind) in items {
            let snapshot = match kind {
                LinkKind::Issue => {
                    let forge = self.issue_url_forge(&url).await?;
                    let (number, _) = forge.issue(&url)?;
                    forge.issue_activity(&account, number).await?
                }
                LinkKind::Pr => {
                    let forge = self.pull_forge(&url).await?;
                    let (number, _) = forge.pull(&url)?;
                    let branch = selection
                        .input
                        .is_none()
                        .then_some(workspace.branch.as_str());
                    forge.activity(&account, number, branch).await?
                }
                LinkKind::Ci => {
                    let forge = self.run_url_forge(&url).await?;
                    forge.run_activity(&account, &url).await?
                }
            };
            observations.push((url, snapshot));
        }
        Ok(observations)
    }

    /// Poll linked items for integrations, with their own baseline, and queue
    /// the changes on each workspace's cursor.
    pub(crate) async fn poll_linked_item_activity(&self) -> Result<()> {
        let mut failure = None;
        for workspace in self.list_workspaces().await? {
            if workspace.state != crate::state::WorkspaceState::Ready {
                continue;
            }
            let id = workspace.id.clone();
            let items = self.store.run(move |db| polled_items(db, &id)).await?;
            if items.is_empty() {
                continue;
            }
            let Err(error) = self.poll_workspace_item_activity(&workspace, items).await else {
                continue;
            };
            // The limit is the forge's, not the workspace's; a later poll retries.
            if self.notify_rate_limit(&error).await {
                continue;
            }
            if failure.is_none() {
                failure = Some(error.context(format!(
                    "background item activity for workspace {}",
                    workspace.name
                )));
            }
        }
        if let Some(error) = failure {
            Err(error)
        } else {
            Ok(())
        }
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

    async fn poll_workspace_item_activity(
        &self,
        workspace: &Workspace,
        items: Vec<(String, LinkKind)>,
    ) -> Result<()> {
        let observed_at = crate::time::unix_seconds();
        let items = self.items_to_look_up(workspace, items).await?;
        let observations = self
            .item_activity(workspace, items, &Selection::default())
            .await?;
        self.record_background_activity(&workspace.id, observations.clone())
            .await?;
        self.queue_background_updates(&workspace.id, observations, observed_at)
            .await
    }

    /// Queue `observations` on the workspace's cursor, so its agent receives
    /// them without a running watch, and wake the watches that wait for them.
    async fn queue_background_updates(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
        observed_at: u64,
    ) -> Result<()> {
        let updates = self
            .record_observed(id, observations, &Selection::default(), observed_at)
            .await?;
        if !updates.is_empty() {
            self.signals.item_updates_queued();
        }
        Ok(())
    }

    async fn record_background_activity(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &id)?;
                let linked = polled_items(&tx, &id)?;
                for (url, kind, mut snapshot) in observations.into_iter().filter_map(
                    |(url, snapshot)| {
                        linked
                            .iter()
                            .find(|(linked_url, _)| linked_url == &url)
                            .map(|(_, kind)| (url, *kind, snapshot))
                    },
                ) {
                    let previous: Option<String> = tx
                        .query_row(
                            "SELECT record FROM workspace_item_activity WHERE workspace_id=?1 AND url=?2",
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
                            crate::daemon::events::record_item_activity(
                                &tx, &id, kind, &url, &update,
                            )?;
                        }
                    }
                    tx.execute(
                        "INSERT INTO workspace_item_activity(workspace_id,url,record)
                         VALUES (?1,?2,?3)
                         ON CONFLICT(workspace_id,url) DO UPDATE SET record=excluded.record",
                        params![id, url, serde_json::to_string(&snapshot)?],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// The canonical URL of an explicitly selected item, linked or not.
    pub(crate) async fn explicit_item(
        &self,
        workspace: &Workspace,
        selection: &Selection,
    ) -> Result<Option<(String, LinkKind)>> {
        let Some(input) = &selection.input else {
            return Ok(None);
        };
        let remote = repository::remote_url_from_path(&workspace.path)
            .await?
            .context("item lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let kind = selection.kind.context("explicit items need a kind")?;
        let (_, url) = match kind {
            LinkKind::Pr => forge.pull(input)?,
            LinkKind::Issue => forge.issue(input)?,
            LinkKind::Ci => forge.run(input)?,
        };
        Ok(Some((url, kind)))
    }

    /// The explicitly selected item, or the workspace's linked items of the
    /// selected kind, with every linked run.
    pub(crate) async fn selected_items(
        &self,
        workspace: &Workspace,
        selection: &Selection,
    ) -> Result<Vec<(String, LinkKind)>> {
        let items = self
            .selection_items(workspace, selection, shown_items)
            .await?;
        ensure!(
            !items.is_empty() || selection.kind.is_none(),
            "{NO_LINKED_ITEMS}"
        );
        Ok(items)
    }

    /// The workspace's own items a watch selects. A kind it links none of is
    /// still watched when a child workspace whose activity its agent receives
    /// links one.
    async fn watched_items(
        &self,
        workspace: &Workspace,
        selection: &Selection,
    ) -> Result<Vec<(String, LinkKind)>> {
        let items = self
            .selection_items(workspace, selection, followed_items)
            .await?;
        if items.is_empty() && selection.input.is_none() {
            let (id, kind) = (workspace.id.clone(), selection.kind);
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

    /// The explicitly selected item, or the workspace's linked items of the
    /// selected kind that `linked` lists.
    async fn selection_items(
        &self,
        workspace: &Workspace,
        selection: &Selection,
        linked: LinkedItems,
    ) -> Result<Vec<(String, LinkKind)>> {
        if let Some(item) = self.explicit_item(workspace, selection).await? {
            return Ok(vec![item]);
        }
        let id = workspace.id.clone();
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
        self.record_observed(id, observations, selection, crate::time::unix_seconds())
            .await
    }

    /// Record `observations` from a poll that started at `observed_at`.
    async fn record_observed(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
        selection: &Selection,
        observed_at: u64,
    ) -> Result<Vec<Update>> {
        let selection = selection.clone();
        let _guard = self.gates.lock_prs([id]).await;
        let id = id.to_owned();
        let agents = self.tracked_agents().await;
        self.store.run(move |db| {
            let tx = db.transaction()?;
            store::require_ready(&tx, &id)?;
            let items = followed_items(&tx, &id, selection.kind)?;
            if selection.input.is_none() && items.is_empty() {
                ensure!(links_kind(&tx, &id, selection.kind, &agents)?, "{NO_LINKED_ITEMS}");
            }
            // A PR linked since the poll takes over conflict reports from the branch.
            let branch = if watches_branch(&selection, &items) {
                Some(tx.query_row("SELECT branch FROM workspaces WHERE id=?1", [&id], |row| row.get::<_, String>(0))?)
            } else {
                None
            };
            let mut updates = Vec::new();
            for (url, mut snapshot) in observations {
                if selection.input.is_none() && !items.iter().any(|(linked, _)| linked == &url) && branch.as_ref() != Some(&url) {
                    continue;
                }
                let previous: Option<String> = tx.query_row(
                    "SELECT record FROM pr_activity WHERE workspace_id=?1 AND url=?2",
                    params![id, url], |row| row.get(0),
                ).optional()?;
                let mut previous: ActivityRecord = previous.map(|text| serde_json::from_str(&text)).transpose()?.unwrap_or_default();
                if !previous.pending.is_empty() {
                    updates.extend(previous.pending);
                    continue;
                }
                snapshot.normalize(&previous.snapshot, crate::time::unix_seconds());
                previous.snapshot.adopt_authored(&snapshot, &mut previous.authored);
                // A poll that started by the agent's own state change may
                // have read the state before it.
                if previous.own_state_at.is_some_and(|at| observed_at <= at) {
                    snapshot.adopt_state(previous.snapshot.state());
                } else {
                    previous.own_state_at = None;
                }
                let changes = previous.snapshot.changes(&snapshot, &url);
                let mut pending = previous.held.deliver(&snapshot, changes);
                let delivery = uuid::Uuid::new_v4().to_string();
                for update in &mut pending {
                    update.delivery.clone_from(&delivery);
                }
                updates.extend(pending.clone());
                let record = ActivityRecord { snapshot, pending, authored: previous.authored, own_state_at: previous.own_state_at, held: previous.held };
                tx.execute("INSERT INTO pr_activity(workspace_id,url,record) VALUES (?1,?2,?3) ON CONFLICT(workspace_id,url) DO UPDATE SET record=excluded.record", params![id, url, serde_json::to_string(&record)?])?;
            }
            tx.commit()?;
            Ok(updates)
        }).await
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
            .run(move |db| {
                let mut statement =
                    db.prepare("SELECT url,record FROM pr_activity WHERE workspace_id=?1")?;
                let records = statement.query_map([id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                let mut updates = Vec::new();
                for record in records {
                    let (url, record) = record?;
                    if !urls.contains(&url) {
                        continue;
                    }
                    let record: ActivityRecord = serde_json::from_str(&record)?;
                    updates.extend(record.pending);
                }
                Ok(updates)
            })
            .await
    }

    pub async fn acknowledge_pr_updates(
        &self,
        selector: &str,
        deliveries: Vec<String>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        // Without the PR gate: an agent's hook acknowledges within its timeout.
        // One transaction removes exactly the acknowledged deliveries, so
        // whatever a poll or a split queues meanwhile stays pending.
        let waits = self.item_waits.clone();
        let agents = self.tracked_agents().await;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &workspace.id)?;
                // The agent also received the updates of the child workspaces
                // it works on.
                let delivered =
                    crate::daemon::workspace::receiving_paths(&tx, &workspace.id, &agents)?;
                // A delivering wait has ended, so a wait running in one of
                // these workspaces, or in one receiving their activity above
                // them, started after it and must report their updates as well.
                waits.with_unwatched(&delivered, |unwatched| {
                    acknowledge_deliveries(&tx, unwatched, &deliveries)?;
                    Ok(tx.commit()?)
                })
            })
            .await?;
        self.signals.agent_changed();
        Ok(())
    }
}

/// The IDs of workspace `id` and of `children`, whose activity its agent
/// receives: the PR gates a delivery to that agent holds.
fn receiving<'a>(id: &'a str, children: &'a [WorkspaceRef]) -> impl Iterator<Item = &'a str> {
    children.iter().map(|child| child.id.as_str()).chain([id])
}

/// A change the workspace's agent made to `url` through Shoal, which its
/// watches do not report back: the comments, replies or reviews it posted
/// and the state it set.
pub(crate) struct OwnChange<'a> {
    pub url: &'a str,
    pub authored: Vec<String>,
    pub state: Option<&'a str>,
}

/// Record `change` on the workspace's cursor. The caller holds the workspace's
/// PR gate from before the change, so no poll that observed it can record it
/// first.
pub(crate) fn record_own_change(
    db: &rusqlite::Connection,
    workspace_id: &str,
    change: OwnChange<'_>,
) -> Result<()> {
    let previous: Option<String> = db
        .query_row(
            "SELECT record FROM pr_activity WHERE workspace_id=?1 AND url=?2",
            params![workspace_id, change.url],
            |row| row.get(0),
        )
        .optional()?;
    let mut record: ActivityRecord = previous
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .unwrap_or_default();
    record.authored.extend(change.authored);
    if let Some(state) = change.state {
        record.snapshot.adopt_state(state);
        record.own_state_at = Some(crate::time::unix_seconds());
    }
    db.execute(
        "INSERT INTO pr_activity(workspace_id,url,record) VALUES (?1,?2,?3)
         ON CONFLICT(workspace_id,url) DO UPDATE SET record=excluded.record",
        params![workspace_id, change.url, serde_json::to_string(&record)?],
    )?;
    Ok(())
}

/// Queue an update for the workspace's next watch of `update.url`, ahead of
/// newly observed activity.
pub(super) fn queue_update(
    db: &rusqlite::Connection,
    workspace_id: &str,
    update: Update,
) -> Result<()> {
    let previous: Option<String> = db
        .query_row(
            "SELECT record FROM pr_activity WHERE workspace_id=?1 AND url=?2",
            params![workspace_id, update.url],
            |row| row.get(0),
        )
        .optional()?;
    let mut record: ActivityRecord = previous
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .unwrap_or_default();
    let url = update.url.clone();
    record.pending.push(update);
    db.execute(
        "INSERT INTO pr_activity(workspace_id,url,record) VALUES (?1,?2,?3)
         ON CONFLICT(workspace_id,url) DO UPDATE SET record=excluded.record",
        params![workspace_id, url, serde_json::to_string(&record)?],
    )?;
    Ok(())
}

/// Remove `deliveries` from the pending updates of the workspaces `ids`;
/// updates queued after them stay pending.
fn acknowledge_deliveries(
    db: &rusqlite::Connection,
    ids: &[String],
    deliveries: &[String],
) -> Result<()> {
    for (id, url, record) in activity_records(db, ids)? {
        let mut record: ActivityRecord = serde_json::from_str(&record)?;
        let pending = record.pending.len();
        record
            .pending
            .retain(|update| !deliveries.contains(&update.delivery));
        if record.pending.len() != pending {
            db.execute(
                "UPDATE pr_activity SET record=?1 WHERE workspace_id=?2 AND url=?3",
                params![serde_json::to_string(&record)?, id, url],
            )?;
        }
    }
    Ok(())
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
    for (id, url, record) in activity_records(db, &[workspace.to_owned()])? {
        let mut record: ActivityRecord = serde_json::from_str(&record)?;
        let mut moved = false;
        for update in &mut record.pending {
            if update.delivery == delivery {
                seen += 1;
                if seen > kept {
                    update.delivery.clone_from(&next);
                    moved = true;
                }
            }
        }
        if moved {
            db.execute(
                "UPDATE pr_activity SET record=?1 WHERE workspace_id=?2 AND url=?3",
                params![serde_json::to_string(&record)?, id, url],
            )?;
        }
    }
    Ok(())
}

/// A workspace branch's local merge check, observed like PR mergeability.
fn branch_conflicts(check: Result<ConflictCheck>) -> Snapshot {
    Snapshot::merge_check(check.map(|check| MergeCheck {
        conflict: check.conflicts.then(|| conflict_message(&check)),
        head: check.head,
        base: check.target_commit,
    }))
}

/// The conflict with the check's target, naming the first conflicted paths.
fn conflict_message(check: &ConflictCheck) -> String {
    let mut files = check.files[..check.files.len().min(MAX_LISTED_FILES)].join(", ");
    if check.files.len() > MAX_LISTED_FILES {
        files.push_str(&format!(
            " and {} more",
            check.files.len() - MAX_LISTED_FILES
        ));
    }
    format!("Conflicts with {}: {files}", check.target)
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

/// The activity record of every item of the workspaces `ids`, with its
/// workspace and URL.
fn activity_records(
    db: &rusqlite::Connection,
    ids: &[String],
) -> Result<Vec<(String, String, String)>> {
    let mut statement =
        db.prepare("SELECT ?1, url, record FROM pr_activity WHERE workspace_id=?1")?;
    let mut records = Vec::new();
    for id in ids {
        records.extend(
            statement
                .query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    Ok(records)
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
            "SELECT url FROM workspace_items WHERE workspace_id=?1 AND kind=?2
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
        let record: Option<String> = db
            .query_row(
                "SELECT record FROM pr_activity WHERE workspace_id=?1 AND url=?2",
                params![id, url],
                |row| row.get(0),
            )
            .optional()?;
        let record: Option<ActivityRecord> =
            record.map(|text| serde_json::from_str(&text)).transpose()?;
        if record.is_some_and(|record| record.pending.is_empty() && record.snapshot.run_finished())
        {
            delivered.push(url.clone());
        }
    }
    runs.retain(|url| !delivered.contains(url));
    Ok(runs)
}

/// The workspace's linked runs the background poll has not seen finish.
fn unpolled_runs(db: &rusqlite::Connection, id: &str) -> Result<Vec<String>> {
    let mut runs = linked_runs(db, id)?;
    let mut finished = Vec::new();
    for url in &runs {
        if observed_item(db, id, url)?.is_some_and(|snapshot| snapshot.run_finished()) {
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
                "SELECT record FROM pr_cleanup WHERE workspace_id=?1",
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
                "SELECT url FROM workspace_issue WHERE workspace_id=?1",
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
    for (url, _) in linked_items(db, id, Some(ItemKind::Pr))? {
        if observed_item(db, id, &url)?.is_none_or(|snapshot| !concluded(&snapshot)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The item as the workspace's last poll saw it, if one has.
fn observed_item(db: &rusqlite::Connection, id: &str, url: &str) -> Result<Option<Snapshot>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM workspace_item_activity WHERE workspace_id=?1 AND url=?2",
            params![id, url],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

/// Whether the workspace's watch saw the item's activity end: a PR merged,
/// or, once the assignment is `done`, an item closed.
fn activity_ended(db: &rusqlite::Connection, id: &str, url: &str, done: bool) -> Result<bool> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM pr_activity WHERE workspace_id=?1 AND url=?2",
            params![id, url],
            |row| row.get(0),
        )
        .optional()?;
    let Some(record) = record else {
        return Ok(false);
    };
    let snapshot = serde_json::from_str::<ActivityRecord>(&record)?.snapshot;
    Ok(snapshot.state() == ItemState::Merged.as_str() || (done && concluded(&snapshot)))
}

/// Whether the item was seen merged or closed, ending its activity.
fn concluded(snapshot: &Snapshot) -> bool {
    [ItemState::Merged, ItemState::Closed]
        .iter()
        .any(|state| snapshot.state() == state.as_str())
}

impl Updates {
    pub(crate) fn superseded() -> Self {
        Self {
            updates: Vec::new(),
            messages: Vec::new(),
            timed_out: false,
            superseded: true,
            attached: None,
        }
    }

    pub(crate) fn messages(messages: Vec<AgentMessage>) -> Self {
        Self {
            updates: Vec::new(),
            messages,
            timed_out: false,
            superseded: false,
            attached: None,
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
/// its base: only an unfiltered wait without a linked PR, whose forge would
/// report them otherwise.
fn watches_branch(selection: &Selection, items: &[(String, LinkKind)]) -> bool {
    selection.input.is_none()
        && selection.kind.is_none()
        && !items.iter().any(|(_, kind)| *kind == LinkKind::Pr)
}

/// Poll every [`POLL_INTERVAL`], or once a forge's rate limit ends, and at
/// once whenever `queued` changes.
async fn wait_for_updates<F: std::future::Future<Output = Result<Vec<Update>>>>(
    seconds: Option<u64>,
    queued: &mut tokio::sync::watch::Receiver<u64>,
    mut poll: impl FnMut() -> F,
) -> Result<Updates> {
    let deadline =
        seconds.map(|seconds| tokio::time::Instant::now() + Duration::from_secs(seconds));
    loop {
        // A cancelled poll may already have committed a pending delivery. The
        // next wait replays it, so the advertised timeout can remain strict.
        let polled = match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, poll()).await {
                Ok(polled) => polled,
                Err(_) => {
                    return Ok(Updates {
                        updates: Vec::new(),
                        messages: Vec::new(),
                        timed_out: true,
                        superseded: false,
                        attached: None,
                    });
                }
            },
            None => poll().await,
        };
        let (updates, interval) = limited_poll(polled, crate::time::unix_seconds())?;
        if !updates.is_empty() {
            return Ok(Updates {
                updates,
                messages: Vec::new(),
                timed_out: false,
                superseded: false,
                attached: None,
            });
        }
        let sleep_for = if let Some(deadline) = deadline {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(Updates {
                    updates: Vec::new(),
                    messages: Vec::new(),
                    timed_out: true,
                    superseded: false,
                    attached: None,
                });
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
            let wait = limit.until.saturating_sub(now);
            Ok((Vec::new(), POLL_INTERVAL.max(Duration::from_secs(wait))))
        }
    }
}

#[cfg(test)]
mod tests;
