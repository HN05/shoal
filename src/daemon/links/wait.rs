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

use super::pr::{RegistrationKind, current_head};
use crate::{
    daemon::{agent_messages::AgentMessage, store, workspace::Manager},
    forge::{
        ForgeRepo,
        link::{ItemKind, Selection},
        updates::{MergeCheck, Snapshot, Update},
    },
    model::{ConflictCheck, Workspace},
    repository,
};

const POLL_INTERVAL: Duration = Duration::from_secs(30);

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
}

/// The running wait of each workspace, identified by its supersession signal.
/// Waits share one cursor per item, so a newer wait supersedes the older one
/// instead of competing for its updates.
#[derive(Default, Clone)]
pub(crate) struct ActiveWaits(Arc<Mutex<HashMap<String, Arc<Notify>>>>);

struct ActiveWait<'a> {
    waits: &'a ActiveWaits,
    workspace: String,
    superseded: Arc<Notify>,
}

impl ActiveWaits {
    fn start(&self, workspace: &str) -> ActiveWait<'_> {
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

impl ActiveWait<'_> {
    fn is_current_in(&self, waits: &HashMap<String, Arc<Notify>>) -> bool {
        waits
            .get(&self.workspace)
            .is_some_and(|current| Arc::ptr_eq(current, &self.superseded))
    }

    fn is_current(&self) -> bool {
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
}

impl Manager {
    #[cfg(test)]
    pub async fn wait_prs(&self, selector: &str, seconds: Option<u64>) -> Result<Updates> {
        self.wait_items(
            selector,
            &Selection {
                kind: Some(ItemKind::Pr),
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
        let mut queued = self.agent_messages_changed.subscribe();
        let mut background = self.item_updates_queued.subscribe();
        let mut messages = self.agent_messages(&workspace.id).await?;
        if messages.is_empty() {
            // A cancelled poll leaves its deliveries pending for the newer wait.
            tokio::select! {
                updates = wait_for_updates(seconds, &mut background, || {
                    self.poll_item_activity(&workspace.id, selection)
                }) => {
                    let updates = updates?;
                    if !wait.is_current() {
                        return Ok(Updates::superseded());
                    }
                    return Ok(updates);
                }
                next = self.next_agent_messages(&workspace.id, &mut queued) => messages = next?,
                () = wait.superseded.notified() => return Ok(Updates::superseded()),
            }
        }
        // A newer wait or hook delivery reads the same unacknowledged messages.
        if !wait.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(Updates {
            updates: Vec::new(),
            messages,
            timed_out: false,
            superseded: false,
        })
    }

    /// What the workspace's agent has not received: its messages and the
    /// updates queued on its cursor, without waiting or polling the forge.
    /// Delivery replaces a running watch, which would otherwise deliver the
    /// same updates again.
    pub async fn agent_updates(&self, selector: &str) -> Result<Updates> {
        let workspace = self.workspace(selector).await?;
        let delivery = self.item_waits.start(&workspace.id);
        let messages = self.agent_messages(&workspace.id).await?;
        // The messages share the response with the updates.
        let reserved = serde_json::to_vec(&messages)?.len();
        let updates = self.agent_queued_updates(workspace, reserved).await?;
        if !delivery.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(Updates {
            messages,
            updates,
            timed_out: false,
            superseded: false,
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
        let _gate = self.pr_gate.lock().await;
        let id = workspace.id.clone();
        let mut batch = own_batch(&id, self.queued_updates(workspace).await?);
        batch.extend(self.delegated_pending(&id, None).await?);
        self.fit_response(batch, reserved).await
    }

    /// The leading updates of `batch` that fit one response beside `reserved`
    /// bytes of its other content. Updates sharing
    /// a delivery are acknowledged together, so a delivery is cut only when
    /// it comes first and does not fit alone; its remainder then moves to a
    /// new delivery and stays pending once the sent part is acknowledged. The
    /// caller holds `pr_gate`.
    async fn fit_response(&self, mut batch: Batch, reserved: usize) -> Result<Vec<Update>> {
        let limit = (crate::protocol::MAX_FRAME - 256).saturating_sub(reserved);
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

    /// The updates queued on the cursors of the child workspaces whose
    /// activity `id`'s agent receives, for their items of `kind` or every
    /// kind, each naming its workspace.
    async fn delegated_pending(&self, id: &str, kind: Option<ItemKind>) -> Result<Batch> {
        let mut updates = Vec::new();
        for child in self.delegated_workspaces(id).await? {
            let child_id = child.id.clone();
            let keys = self
                .store
                .run(move |db| linked_items(db, &child_id, kind))
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
            .run(move |db| linked_items(db, &id, None))
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
    /// messages or queued updates matches `due`, so a launcher can prompt an
    /// agent that has no hook to wake it, then wait for it to take them.
    pub async fn wait_agent_wake(&self, selector: &str, due: bool) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        // Subscribe before reading, so a change in between still wakes.
        let mut messages = self.agent_messages_changed.subscribe();
        let mut queued = self.item_updates_queued.subscribe();
        let mut changed = self.agent_wake_changed.subscribe();
        while self.agent_wake_due(&workspace).await? != due {
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

    async fn agent_wake_due(&self, workspace: &Workspace) -> Result<bool> {
        let id = workspace.id.clone();
        let state = self
            .store
            .run(move |db| crate::daemon::workspace::agent_state::get(db, &id))
            .await?;
        if state.is_none_or(|status| status.state != crate::state::AgentState::Idle) {
            return Ok(false);
        }
        Ok(!self.agent_messages(&workspace.id).await?.is_empty()
            || !self
                .agent_queued_updates(workspace.clone(), 0)
                .await?
                .is_empty())
    }

    async fn next_agent_messages(
        &self,
        id: &str,
        queued: &mut tokio::sync::watch::Receiver<u64>,
    ) -> Result<Vec<AgentMessage>> {
        loop {
            queued.changed().await?;
            let messages = self.agent_messages(id).await?;
            if !messages.is_empty() {
                return Ok(messages);
            }
        }
    }

    async fn poll_item_activity(&self, id: &str, selection: &Selection) -> Result<Vec<Update>> {
        let guard = self.pr_gate.lock().await;
        let workspace = self.workspace(id).await?;
        self.verify_worktree(&workspace).await?;
        current_head(&workspace).await?;
        let items = self.watched_items(&workspace, selection).await?;
        let branch = watches_branch(selection, &items).then(|| workspace.branch.clone());
        let mut keys = items.iter().map(|(url, _)| url.clone()).collect::<Vec<_>>();
        keys.extend(branch.clone());
        let mut batch = own_batch(id, self.pending_activity(id, &keys).await?);
        if selection.input.is_none() {
            batch.extend(self.delegated_pending(id, selection.kind).await?);
        }
        // A wait returns messages on their own, without updates.
        let mut pending = self.fit_response(batch, 0).await?;
        if !pending.is_empty() {
            drop(guard);
            self.summarize_failed_checks(id, &mut pending).await;
            return Ok(pending);
        }
        drop(guard);
        let observed_at = crate::time::unix_seconds();
        let items = self.item_activity(&workspace, items, selection).await;
        let mut observations = Vec::new();
        if let Some(branch) = branch {
            let check = self.check_conflicts(id, None).await;
            observations.push((branch, branch_conflicts(check)));
        }
        match items {
            Ok(items) => {
                observations.splice(0..0, items);
                let mut updates = self
                    .record_observed(id, observations, selection, observed_at)
                    .await?;
                self.summarize_failed_checks(id, &mut updates).await;
                Ok(updates)
            }
            // A failed item lookup must not hold back the branch's conflicts.
            Err(error) => {
                let updates = self
                    .record_observed(id, observations, selection, observed_at)
                    .await?;
                if updates.is_empty() {
                    return Err(error);
                }
                Ok(updates)
            }
        }
    }

    async fn item_activity(
        &self,
        workspace: &Workspace,
        items: Vec<(String, ItemKind)>,
        selection: &Selection,
    ) -> Result<Vec<(String, Snapshot)>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let remote = repository::remote_url_from_path(&workspace.path)
            .await?
            .context("watch needs an origin remote")?;
        let forge = ForgeRepo::parse(&remote)?;
        let account = self.user_account();
        let mut observations = Vec::new();
        for (url, kind) in items {
            let snapshot = match kind {
                ItemKind::Issue => {
                    let (number, _) = forge.issue(&url)?;
                    forge.issue_activity(&account, number).await?
                }
                ItemKind::Pr => {
                    let (number, _) = forge.pull(&url)?;
                    let branch = selection
                        .input
                        .is_none()
                        .then_some(workspace.branch.as_str());
                    forge.activity(&account, number, branch).await?
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
            let items = self
                .store
                .run(move |db| linked_items(db, &id, None))
                .await?;
            if items.is_empty() {
                continue;
            }
            if let Err(error) = self.poll_workspace_item_activity(&workspace, items).await
                && failure.is_none()
            {
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

    async fn poll_workspace_item_activity(
        &self,
        workspace: &Workspace,
        items: Vec<(String, ItemKind)>,
    ) -> Result<()> {
        self.verify_worktree(workspace).await?;
        let observed_at = crate::time::unix_seconds();
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
            self.item_updates_queued.send_modify(|count| *count += 1);
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
                let linked = linked_items(&tx, &id, None)?;
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
                    if let Some(previous) = previous {
                        let previous: Snapshot = serde_json::from_str(&previous)?;
                        snapshot.retain_failed_checks(&previous);
                        snapshot.schedule_conflict_reports(&previous);
                        snapshot.schedule_failure_reports(
                            &previous,
                            crate::time::unix_seconds(),
                        );
                        for update in previous.changes(&snapshot, &url) {
                            crate::daemon::events::record_item_activity(
                                &tx, &id, kind, &url, &update,
                            )?;
                        }
                    } else {
                        snapshot.schedule_conflict_reports(&Snapshot::default());
                        snapshot.schedule_failure_reports(
                            &Snapshot::default(),
                            crate::time::unix_seconds(),
                        );
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
    ) -> Result<Option<(String, ItemKind)>> {
        let Some(input) = &selection.input else {
            return Ok(None);
        };
        let remote = repository::remote_url_from_path(&workspace.path)
            .await?
            .context("item lookup needs an origin remote")?;
        let forge = ForgeRepo::parse(&remote)?;
        let kind = selection.kind.context("explicit items need a kind")?;
        let (_, url) = match kind {
            ItemKind::Pr => forge.pull(input)?,
            ItemKind::Issue => forge.issue(input)?,
        };
        Ok(Some((url, kind)))
    }

    pub(crate) async fn selected_items(
        &self,
        workspace: &Workspace,
        selection: &Selection,
    ) -> Result<Vec<(String, ItemKind)>> {
        let items = self.selection_items(workspace, selection).await?;
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
    ) -> Result<Vec<(String, ItemKind)>> {
        let items = self.selection_items(workspace, selection).await?;
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
    /// selected kind.
    async fn selection_items(
        &self,
        workspace: &Workspace,
        selection: &Selection,
    ) -> Result<Vec<(String, ItemKind)>> {
        if let Some(item) = self.explicit_item(workspace, selection).await? {
            return Ok(vec![item]);
        }
        let id = workspace.id.clone();
        let kind = selection.kind;
        self.store.run(move |db| linked_items(db, &id, kind)).await
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
                kind: Some(ItemKind::Pr),
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
        let _guard = self.pr_gate.lock().await;
        let id = id.to_owned();
        let agents = self.tracked_agents().await;
        self.store.run(move |db| {
            let tx = db.transaction()?;
            store::require_ready(&tx, &id)?;
            let items = linked_items(&tx, &id, selection.kind)?;
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
                snapshot.retain_failed_checks(&previous.snapshot);
                snapshot.schedule_conflict_reports(&previous.snapshot);
                snapshot.schedule_failure_reports(&previous.snapshot, crate::time::unix_seconds());
                previous.snapshot.adopt_authored(&snapshot, &mut previous.authored);
                // A poll that started by the agent's own state change may
                // have read the state before it.
                if previous.own_state_at.is_some_and(|at| observed_at <= at) {
                    snapshot.adopt_state(previous.snapshot.state());
                } else {
                    previous.own_state_at = None;
                }
                let mut pending = previous.snapshot.changes(&snapshot, &url);
                let delivery = uuid::Uuid::new_v4().to_string();
                for update in &mut pending {
                    update.delivery.clone_from(&delivery);
                }
                updates.extend(pending.clone());
                let record = ActivityRecord { snapshot, pending, authored: previous.authored, own_state_at: previous.own_state_at };
                tx.execute("INSERT INTO pr_activity(workspace_id,url,record) VALUES (?1,?2,?3) ON CONFLICT(workspace_id,url) DO UPDATE SET record=excluded.record", params![id, url, serde_json::to_string(&record)?])?;
            }
            ensure!(serde_json::to_vec(&updates)?.len() < crate::protocol::MAX_FRAME - 256,
                "too many updates for one response; select fewer items");
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
        let _guard = self.pr_gate.lock().await;
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
        self.agent_wake_changed.send_modify(|count| *count += 1);
        Ok(())
    }
}

/// A change the workspace's agent made to `url` through Shoal, which its
/// watches do not report back: the comment or reply it posted and the state
/// it set.
pub(crate) struct OwnChange<'a> {
    pub url: &'a str,
    pub authored: Option<String>,
    pub state: Option<&'a str>,
}

/// Record `change` on the workspace's cursor. The caller holds `pr_gate` from
/// before the change, so no poll that observed it can record it first.
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
    kind: Option<ItemKind>,
    agents: &crate::daemon::workspace::TrackedAgents,
) -> Result<bool> {
    if kind.is_none() || !linked_items(db, id, kind)?.is_empty() {
        return Ok(true);
    }
    for child in crate::daemon::workspace::delegated(db, id, agents)? {
        if !linked_items(db, &child.id, kind)?.is_empty() {
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

impl Updates {
    fn superseded() -> Self {
        Self {
            updates: Vec::new(),
            messages: Vec::new(),
            timed_out: false,
            superseded: true,
        }
    }
}

/// Whether a wait also reports the workspace branch's local conflicts with
/// its base: only an unfiltered wait without a linked PR, whose forge would
/// report them otherwise.
fn watches_branch(selection: &Selection, items: &[(String, ItemKind)]) -> bool {
    selection.input.is_none()
        && selection.kind.is_none()
        && !items.iter().any(|(_, kind)| *kind == ItemKind::Pr)
}

/// Poll every [`POLL_INTERVAL`], and at once whenever `queued` changes.
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
        let updates = match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, poll()).await {
                Ok(updates) => updates?,
                Err(_) => {
                    return Ok(Updates {
                        updates: Vec::new(),
                        messages: Vec::new(),
                        timed_out: true,
                        superseded: false,
                    });
                }
            },
            None => poll().await?,
        };
        if !updates.is_empty() {
            return Ok(Updates {
                updates,
                messages: Vec::new(),
                timed_out: false,
                superseded: false,
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
                });
            }
            POLL_INTERVAL.min(deadline - now)
        } else {
            POLL_INTERVAL
        };
        // A closed channel only disables the early wake.
        tokio::select! {
            () = tokio::time::sleep(sleep_for) => {}
            Ok(()) = queued.changed() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        forge::updates::UpdateKind,
        test_support::{manager, repository},
    };
    use serde_json::{Value, json};

    #[tokio::test(start_paused = true)]
    async fn polling_wakes_for_a_single_result_and_timeout_has_no_update() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let result = wait_for_updates(
            Some(3600),
            &mut tokio::sync::watch::channel(0).1,
            || async {
                Ok(
                    if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                        Vec::new()
                    } else {
                        vec![Update {
                            url: "pr".into(),
                            kind: UpdateKind::CiCompleted,
                            message: "review passed".into(),
                            delivery: String::new(),
                            check: None,
                            workspace: None,
                        }]
                    },
                )
            },
        )
        .await
        .unwrap();
        assert!(!result.timed_out);
        assert_eq!(result.updates.len(), 1);
        assert_eq!(tokio::time::Instant::now() - started, POLL_INTERVAL);
        let started = tokio::time::Instant::now();
        let result = wait_for_updates(Some(75), &mut tokio::sync::watch::channel(0).1, || async {
            Ok(Vec::new())
        })
        .await
        .unwrap();
        assert!(result.timed_out && result.updates.is_empty());
        assert_eq!(
            tokio::time::Instant::now() - started,
            Duration::from_secs(75)
        );
        let error = wait_for_updates(
            Some(3600),
            &mut tokio::sync::watch::channel(0).1,
            || async { anyhow::bail!("lookup failed") },
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "lookup failed");
    }

    #[tokio::test(start_paused = true)]
    async fn queued_background_updates_wake_a_wait_before_its_next_poll() {
        let (queue, mut queued) = tokio::sync::watch::channel(0);
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let wait = wait_for_updates(None, &mut queued, || async {
            Ok(
                if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                    queue.send_modify(|count| *count += 1);
                    Vec::new()
                } else {
                    vec![Update {
                        url: "pr".into(),
                        kind: UpdateKind::Comment,
                        message: "queued".into(),
                        delivery: String::new(),
                        check: None,
                        workspace: None,
                    }]
                },
            )
        });
        assert_eq!(wait.await.unwrap().updates[0].message, "queued");
        assert_eq!(tokio::time::Instant::now(), started);
    }

    #[tokio::test(start_paused = true)]
    async fn polling_without_a_timeout_stays_unbounded() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let result = wait_for_updates(None, &mut tokio::sync::watch::channel(0).1, || async {
            if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 120 {
                Ok(vec![Update {
                    url: "pr".into(),
                    kind: UpdateKind::CiCompleted,
                    message: "review passed".into(),
                    delivery: String::new(),
                    check: None,
                    workspace: None,
                }])
            } else {
                Ok(Vec::new())
            }
        })
        .await
        .unwrap();
        assert!(!result.timed_out);
        assert_eq!(result.updates.len(), 1);
        assert_eq!(
            tokio::time::Instant::now() - started,
            Duration::from_secs(3600)
        );
    }

    #[tokio::test]
    async fn a_newer_wait_supersedes_the_older_one_per_workspace() {
        let waits = ActiveWaits::default();
        let older = waits.start("one");
        let other = waits.start("two");
        let newer = waits.start("one");
        older.superseded.notified().await;
        assert!(!older.is_current());
        assert!(newer.is_current() && other.is_current());
        drop(older);
        assert!(newer.is_current());
        drop(newer);
        assert!(!waits.0.lock().unwrap().contains_key("one"));
    }

    #[tokio::test]
    async fn agent_messages_end_a_watch_before_item_activity() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "messages".into(), None, None, None)
            .await
            .unwrap();
        // Waiting would fail without a linked PR; a queued message answers first.
        manager
            .send_agent_message(&workspace.id, "Rebase onto main".into())
            .await
            .unwrap();
        let updates = manager.wait_prs(&workspace.id, Some(60)).await.unwrap();
        assert!(updates.updates.is_empty() && !updates.timed_out);
        assert_eq!(updates.messages[0].message, "Rebase onto main");
        manager
            .mark_agent_messages_delivered(&workspace.id, vec![updates.messages[0].id])
            .await
            .unwrap();
        assert!(manager.wait_prs(&workspace.id, Some(60)).await.is_err());

        let mut queued = manager.agent_messages_changed.subscribe();
        let waiting = {
            let manager = manager.clone();
            let id = workspace.id.clone();
            tokio::spawn(async move { manager.next_agent_messages(&id, &mut queued).await })
        };
        manager
            .send_agent_message(&workspace.id, "Stop the dev server".into())
            .await
            .unwrap();
        let messages = waiting.await.unwrap().unwrap();
        assert_eq!(messages[0].message, "Stop the dev server");
    }

    fn snapshot(message: &str) -> Snapshot {
        serde_json::from_value(json!({"comments": {"one": message}, "checks": {}, "conflict": null, "state": "open", "errors": {}})).unwrap()
    }

    async fn register_watch(manager: &Manager, id: &str, url: &str) {
        let id = id.to_owned();
        let registration = super::super::pr::Registration {
            kind: RegistrationKind::Watch {
                urls: vec![url.into()],
                merged_head: None,
            },
            error: None,
        };
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT OR REPLACE INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
                    params![id, serde_json::to_string(&registration)?],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn background_activity_uses_its_own_baseline_and_publishes_events() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "activity".into(), None, None, None)
            .await
            .unwrap();
        register_watch(&manager, &workspace.id, "pr").await;

        manager
            .record_background_activity(&workspace.id, vec![("pr".into(), snapshot("first"))])
            .await
            .unwrap();
        assert!(manager
            .workspace_events(None, 100)
            .await
            .unwrap()
            .iter()
            .all(|item| !matches!(item, crate::daemon::events::EventItem::Event(event) if event.details.kind == crate::daemon::events::EventKind::ItemActivity)));

        manager
            .record_background_activity(&workspace.id, vec![("pr".into(), snapshot("second"))])
            .await
            .unwrap();
        let events = manager.workspace_events(None, 100).await.unwrap();
        let activity = events.iter().find_map(|item| match item {
            crate::daemon::events::EventItem::Event(event)
                if event.details.kind == crate::daemon::events::EventKind::ItemActivity =>
            {
                event.details.item_activity.as_deref()
            }
            _ => None,
        });
        let activity = activity.expect("background activity event");
        assert_eq!(activity.kind, ItemKind::Pr);
        assert_eq!(activity.url, "pr");
        assert_eq!(activity.update.kind, UpdateKind::Comment);
    }

    #[tokio::test]
    async fn background_activity_queues_for_the_agents_hooks() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "queued".into(), None, None, None)
            .await
            .unwrap();
        register_watch(&manager, &workspace.id, "pr").await;
        let queued = manager.item_updates_queued.subscribe();
        manager
            .queue_background_updates(
                &workspace.id,
                vec![("pr".into(), snapshot("first"))],
                crate::time::unix_seconds(),
            )
            .await
            .unwrap();
        assert!(queued.has_changed().unwrap());
        manager
            .send_agent_message(&workspace.id, "Rebase onto main".into())
            .await
            .unwrap();
        // Handing them to the agent's hook replaces the running watch.
        let watch = manager.item_waits.start(&workspace.id);
        let delivered = manager.agent_updates(&workspace.id).await.unwrap();
        watch.superseded.notified().await;
        assert!(!watch.is_current());
        assert_eq!(delivered.updates[0].kind, UpdateKind::Comment);
        assert_eq!(delivered.messages[0].message, "Rebase onto main");
        manager
            .mark_agent_messages_delivered(&workspace.id, vec![delivered.messages[0].id])
            .await
            .unwrap();
        manager
            .acknowledge_pr_updates(&workspace.id, vec![delivered.updates[0].delivery.clone()])
            .await
            .unwrap();
        let after = manager.agent_updates(&workspace.id).await.unwrap();
        assert!(after.updates.is_empty() && after.messages.is_empty());
    }

    #[tokio::test]
    async fn child_activity_reaches_the_parents_agent_until_the_child_has_its_own() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let parent = manager
            .create_workspace(&repo.id, "parent".into(), None, None, None)
            .await
            .unwrap();
        let child = manager
            .split_workspace(&parent.id, "child".into(), None)
            .await
            .unwrap();
        let grandchild = manager
            .split_workspace(&child.id, "grandchild".into(), None)
            .await
            .unwrap();
        let watched = [(&child, "child-pr"), (&grandchild, "grandchild-pr")];
        for (workspace, url) in watched {
            register_watch(&manager, &workspace.id, url).await;
        }
        // The parent links no PR, but its children do.
        let prs = Selection {
            kind: Some(ItemKind::Pr),
            input: None,
        };
        // A poll records the parent's own observations without refusing the kind.
        let quiet = manager.poll_item_activity(&parent.id, &prs).await.unwrap();
        assert!(quiet.is_empty());
        // Queued updates end a wait at once; the timeout only guards a hang.
        let watch = |selection: Selection| {
            let manager = &manager;
            let id = parent.id.clone();
            async move {
                tokio::time::timeout(
                    Duration::from_secs(60),
                    manager.wait_items(&id, &selection, None),
                )
                .await
                .expect("watch ended")
            }
        };
        for (workspace, url) in watched {
            manager
                .queue_background_updates(
                    &workspace.id,
                    vec![(url.into(), snapshot("first"))],
                    crate::time::unix_seconds(),
                )
                .await
                .unwrap();
        }
        let delivered = manager.agent_updates(&parent.id).await.unwrap();
        let received: Vec<_> = delivered
            .updates
            .iter()
            .map(|update| (update.workspace.as_deref(), update.url.as_str()))
            .collect();
        assert_eq!(
            received,
            [
                (Some("child"), "child-pr"),
                (Some("grandchild"), "grandchild-pr")
            ]
        );
        assert!(
            delivered.updates[0]
                .to_string()
                .starts_with("child: child-pr [comment]:")
        );
        // An unfiltered watch of the parent reports them as well.
        let unfiltered = watch(Selection::default()).await.unwrap();
        assert_eq!(unfiltered.updates.len(), 2);
        let filtered = watch(prs.clone()).await.unwrap();
        assert_eq!(filtered.updates.len(), 2);

        // An agent of the child's own takes over the child's subtree.
        manager
            .set_agent_state(&child.id, crate::state::AgentState::Working, None, None)
            .await
            .unwrap();
        assert!(
            manager
                .agent_updates(&parent.id)
                .await
                .unwrap()
                .updates
                .is_empty()
        );
        let refused = watch(prs.clone()).await;
        assert!(format!("{:#}", refused.unwrap_err()).contains("no linked items"));
        let own = manager.agent_updates(&child.id).await.unwrap();
        assert_eq!(own.updates.len(), 2);
        assert_eq!(own.updates[0].workspace, None);
        assert_eq!(own.updates[1].workspace.as_deref(), Some("grandchild"));
        manager
            .acknowledge_pr_updates(
                &child.id,
                own.updates
                    .iter()
                    .map(|update| update.delivery.clone())
                    .collect(),
            )
            .await
            .unwrap();
        assert!(
            manager
                .agent_updates(&child.id)
                .await
                .unwrap()
                .updates
                .is_empty()
        );
        assert!(
            manager
                .agent_updates(&grandchild.id)
                .await
                .unwrap()
                .updates
                .is_empty()
        );
    }

    #[tokio::test]
    async fn acknowledging_child_updates_leaves_them_to_a_running_child_wait() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let parent = manager
            .create_workspace(&repo.id, "parent".into(), None, None, None)
            .await
            .unwrap();
        let child = manager
            .split_workspace(&parent.id, "child".into(), None)
            .await
            .unwrap();
        for (workspace, url) in [(&parent, "parent-pr"), (&child, "child-pr")] {
            register_watch(&manager, &workspace.id, url).await;
            manager
                .queue_background_updates(
                    &workspace.id,
                    vec![(url.into(), snapshot("first"))],
                    crate::time::unix_seconds(),
                )
                .await
                .unwrap();
        }
        let delivered = manager.agent_updates(&parent.id).await.unwrap();
        assert_eq!(delivered.updates.len(), 2);
        let deliveries = delivered
            .updates
            .iter()
            .map(|update| update.delivery.clone())
            .collect::<Vec<_>>();
        // A newer watch in the parent receives the child's updates as well.
        let parent_wait = manager.item_waits.start(&parent.id);
        manager
            .acknowledge_pr_updates(&parent.id, deliveries.clone())
            .await
            .unwrap();
        assert_eq!(
            manager.pending_pr_activity(&parent.id).await.unwrap().len(),
            1
        );
        assert_eq!(
            manager.pending_pr_activity(&child.id).await.unwrap().len(),
            1
        );
        drop(parent_wait);
        // A watch in the child starts before the parent acknowledges.
        let child_wait = manager.item_waits.start(&child.id);
        manager
            .acknowledge_pr_updates(&parent.id, deliveries.clone())
            .await
            .unwrap();
        assert!(
            manager
                .pending_pr_activity(&parent.id)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            manager.pending_pr_activity(&child.id).await.unwrap().len(),
            1
        );
        drop(child_wait);
        // A watch in the child itself, acknowledged while a newer parent
        // watch runs, leaves the parent's watch the child's updates.
        let parent_wait = manager.item_waits.start(&parent.id);
        manager
            .acknowledge_pr_updates(&child.id, deliveries.clone())
            .await
            .unwrap();
        assert_eq!(
            manager.pending_pr_activity(&child.id).await.unwrap().len(),
            1
        );
        drop(parent_wait);
        manager
            .acknowledge_pr_updates(&parent.id, deliveries)
            .await
            .unwrap();
        assert!(
            manager
                .pending_pr_activity(&child.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    fn large_update(n: usize, delivery: usize) -> Update {
        Update {
            url: "child-pr".into(),
            kind: UpdateKind::Comment,
            message: format!("{n}{}", "x".repeat(4096)),
            delivery: delivery.to_string(),
            check: None,
            workspace: None,
        }
    }

    #[test]
    fn combined_updates_fit_one_response_in_whole_deliveries() {
        const LIMIT: usize = crate::protocol::MAX_FRAME - 256;
        let batch = |deliveries: fn(usize) -> usize| {
            (0..63)
                .map(|n| ("child".to_owned(), large_update(n, deliveries(n))))
                .collect::<Batch>()
        };
        // 21 deliveries of three updates each, more than one response holds.
        let Cut::Between(fitting) = frame_cut(&batch(|n| n / 3), LIMIT) else {
            panic!("whole deliveries fit");
        };
        assert!(fitting > 0 && fitting < 63 && fitting % 3 == 0);
        let sent = batch(|n| n / 3)[..fitting].to_vec();
        assert!(serde_json::to_vec(&sent).unwrap().len() < crate::protocol::MAX_FRAME);
        // One delivery larger than a response is cut inside it.
        let Cut::Inside(fitting) = frame_cut(&batch(|_| 0), LIMIT) else {
            panic!("a delivery too large for a response is cut");
        };
        assert!(fitting > 0 && fitting < 63);
        assert_eq!(frame_cut(&Vec::new(), LIMIT), Cut::Between(0));
    }

    #[tokio::test]
    async fn hook_responses_fit_messages_and_updates_together() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let parent = manager
            .create_workspace(&repo.id, "parent".into(), None, None, None)
            .await
            .unwrap();
        let child = manager
            .split_workspace(&parent.id, "child".into(), None)
            .await
            .unwrap();
        register_watch(&manager, &child.id, "child-pr").await;
        let id = child.id.clone();
        manager
            .store
            .run(move |db| {
                for n in 0..63 {
                    queue_update(db, &id, large_update(n, n))?;
                }
                Ok(())
            })
            .await
            .unwrap();
        // As many unread messages as a workspace holds.
        for n in 0..50 {
            manager
                .send_agent_message(&parent.id, format!("{n:02}{}", "m".repeat(498)))
                .await
                .unwrap();
        }
        let delivered = manager.agent_updates(&parent.id).await.unwrap();
        assert_eq!(delivered.messages.len(), 50);
        assert!(!delivered.updates.is_empty());
        let response =
            crate::protocol::Response::new(1, crate::protocol::Body::PrUpdates(delivered));
        assert!(serde_json::to_vec(&response).unwrap().len() < crate::protocol::MAX_FRAME);
    }

    #[tokio::test]
    async fn a_delivery_too_large_for_one_response_arrives_in_parts() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let parent = manager
            .create_workspace(&repo.id, "parent".into(), None, None, None)
            .await
            .unwrap();
        let child = manager
            .split_workspace(&parent.id, "child".into(), None)
            .await
            .unwrap();
        register_watch(&manager, &child.id, "child-pr").await;
        let id = child.id.clone();
        manager
            .store
            .run(move |db| {
                for n in 0..63 {
                    queue_update(db, &id, large_update(n, 0))?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let mut received = Vec::new();
        while received.len() < 63 {
            let delivered = manager.agent_updates(&parent.id).await.unwrap();
            assert!(!delivered.updates.is_empty() && delivered.updates.len() < 63);
            assert!(serde_json::to_vec(&delivered).unwrap().len() < crate::protocol::MAX_FRAME,);
            manager
                .acknowledge_pr_updates(
                    &parent.id,
                    delivered
                        .updates
                        .iter()
                        .map(|update| update.delivery.clone())
                        .collect(),
                )
                .await
                .unwrap();
            received.extend(delivered.updates);
        }
        let order: Vec<_> = (0..63).map(|n| n.to_string()).collect();
        for (update, n) in received.iter().zip(&order) {
            assert!(update.message.starts_with(&format!("{n}x")), "{n}");
        }
        assert!(
            manager
                .pending_pr_activity(&child.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn deliveries_left_out_of_a_response_stay_pending_whole() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let parent = manager
            .create_workspace(&repo.id, "parent".into(), None, None, None)
            .await
            .unwrap();
        let child = manager
            .split_workspace(&parent.id, "child".into(), None)
            .await
            .unwrap();
        register_watch(&manager, &child.id, "child-pr").await;
        let id = child.id.clone();
        manager
            .store
            .run(move |db| {
                for n in 0..63 {
                    queue_update(db, &id, large_update(n, n / 3))?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let first = manager.agent_updates(&parent.id).await.unwrap();
        assert!(first.updates.len() < 63);
        manager
            .acknowledge_pr_updates(
                &parent.id,
                first
                    .updates
                    .iter()
                    .map(|update| update.delivery.clone())
                    .collect(),
            )
            .await
            .unwrap();
        let rest = manager.pending_pr_activity(&child.id).await.unwrap();
        assert_eq!(first.updates.len() + rest.len(), 63);
        assert_eq!(rest.len() % 3, 0);
        assert!(
            rest[0]
                .message
                .starts_with(&first.updates.len().to_string())
        );
    }

    #[tokio::test]
    async fn agent_wakes_are_due_while_an_idle_agent_has_updates() {
        let (root, manager) = manager().await;
        let manager = Arc::new(manager);
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "wake".into(), None, None, None)
            .await
            .unwrap();
        let wait = |due| {
            let manager = manager.clone();
            let id = workspace.id.clone();
            tokio::spawn(async move { manager.wait_agent_wake(&id, due).await })
        };
        // A hang guard only: each wait ends on the change before it.
        let finish = |task: tokio::task::JoinHandle<Result<()>>| async {
            tokio::time::timeout(Duration::from_secs(60), task)
                .await
                .expect("wait ended")
                .unwrap()
                .unwrap();
        };
        let state = |state| manager.set_agent_state(&workspace.id, state, None, None);
        state(crate::state::AgentState::Working).await.unwrap();
        manager
            .send_agent_message(&workspace.id, "Rebase onto main".into())
            .await
            .unwrap();
        let due = wait(true);
        state(crate::state::AgentState::Idle).await.unwrap();
        finish(due).await;
        let taken = wait(false);
        let messages = manager.agent_messages(&workspace.id).await.unwrap();
        manager
            .mark_agent_messages_delivered(&workspace.id, vec![messages[0].id])
            .await
            .unwrap();
        finish(taken).await;
    }

    #[tokio::test]
    async fn own_changes_are_not_reported_back_to_the_workspace() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "own".into(), None, None, None)
            .await
            .unwrap();
        register_watch(&manager, &workspace.id, "pr").await;
        let observe = |comments: Value, state: &str| {
            let snapshot = serde_json::from_value(json!({"comments": comments, "checks": {},
                "conflict": null, "state": state, "errors": {}}))
            .unwrap();
            manager.record_pr_activity(&workspace.id, vec![("pr".into(), snapshot)])
        };
        let first = observe(json!({"discussion:1": "theirs"}), "open")
            .await
            .unwrap();
        manager
            .acknowledge_pr_updates(&workspace.id, vec![first[0].delivery.clone()])
            .await
            .unwrap();
        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                let change = OwnChange {
                    url: "pr",
                    authored: Some("discussion:2".into()),
                    state: Some("closed"),
                };
                record_own_change(db, &id, change)
            })
            .await
            .unwrap();
        // A poll that read the item before the agent closed it records its
        // comments, but not its stale state.
        let stale = serde_json::from_value(json!({"comments": {"discussion:1": "theirs"},
            "checks": {}, "conflict": null, "state": "open", "errors": {}}))
        .unwrap();
        let all = Selection::default();
        let before = manager.record_observed(&workspace.id, vec![("pr".into(), stale)], &all, 0);
        assert!(before.await.unwrap().is_empty());
        let own = json!({"discussion:1": "theirs", "discussion:2": "mine"});
        assert!(observe(own, "closed").await.unwrap().is_empty());
        let next = json!({"discussion:1": "theirs", "discussion:2": "mine", "discussion:3": "new"});
        let updates = observe(next, "closed").await.unwrap();
        assert_eq!(
            updates.iter().map(|update| update.kind).collect::<Vec<_>>(),
            [UpdateKind::Comment]
        );
    }

    #[tokio::test]
    async fn background_activity_does_not_repeat_an_unchanged_baseline_failure() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "failed-activity".into(), None, None, None)
            .await
            .unwrap();
        register_watch(&manager, &workspace.id, "pr").await;
        let failed: Snapshot = serde_json::from_value(serde_json::json!({
            "comments": {},
            "checks": {},
            "conflict": null,
            "state": "open",
            "errors": {"forge": "temporarily unavailable"}
        }))
        .unwrap();

        manager
            .record_background_activity(&workspace.id, vec![("pr".into(), failed.clone())])
            .await
            .unwrap();
        manager
            .record_background_activity(&workspace.id, vec![("pr".into(), failed)])
            .await
            .unwrap();
        assert!(manager
            .workspace_events(None, 100)
            .await
            .unwrap()
            .iter()
            .all(|item| !matches!(item, crate::daemon::events::EventItem::Event(event) if event.details.kind == crate::daemon::events::EventKind::ItemActivity)));
    }

    #[tokio::test]
    async fn linked_filters_preserve_other_cursors_and_unlink_drops_inflight_activity() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "items".into(), None, None, None)
            .await
            .unwrap();
        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO workspace_issue(workspace_id,url) VALUES (?1,'issue')",
                    [id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        register_watch(&manager, &workspace.id, "pr").await;
        let updates = manager
            .record_activity(
                &workspace.id,
                vec![
                    ("pr".into(), snapshot("review")),
                    ("issue".into(), snapshot("comment")),
                ],
                &Selection::default(),
            )
            .await
            .unwrap();
        assert_eq!(updates.len(), 2);
        let pending = manager
            .pending_activity(&workspace.id, &["pr".into()])
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        manager
            .acknowledge_pr_updates(&workspace.id, vec![pending[0].delivery.clone()])
            .await
            .unwrap();
        manager
            .set_pr(&workspace.id, super::super::pr::Action::Clear)
            .await
            .unwrap();
        assert_eq!(
            manager
                .pending_activity(&workspace.id, &["issue".into()])
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            manager
                .record_activity(
                    &workspace.id,
                    vec![("pr".into(), snapshot("later"))],
                    &Selection::default()
                )
                .await
                .unwrap()
                .is_empty()
        );
        manager.clear_issue(&workspace.id, None).await.unwrap();
        assert!(
            manager
                .pending_activity(&workspace.id, &["issue".into()])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            manager
                .record_activity(
                    &workspace.id,
                    vec![("issue".into(), snapshot("later"))],
                    &Selection::default()
                )
                .await
                .unwrap()
                .is_empty()
        );
        let explicit = Selection {
            kind: Some(ItemKind::Issue),
            input: Some("2".into()),
        };
        let updates = manager
            .record_activity(
                &workspace.id,
                vec![("explicit".into(), snapshot("new"))],
                &explicit,
            )
            .await
            .unwrap();
        assert_eq!(updates.len(), 1);
        assert!(
            manager
                .issue_registration(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            manager
                .pr_registration(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_gate_and_remote_poll_respect_the_timeout() {
        let gate = tokio::sync::Mutex::new(());
        let held = gate.lock().await;
        let started = tokio::time::Instant::now();
        let result = wait_for_updates(Some(1), &mut tokio::sync::watch::channel(0).1, || async {
            let _guard = gate.lock().await;
            Ok(Vec::new())
        })
        .await
        .unwrap();
        assert!(result.timed_out);
        assert!(result.updates.is_empty());
        assert_eq!(
            tokio::time::Instant::now() - started,
            Duration::from_secs(1)
        );
        drop(held);
        let started = tokio::time::Instant::now();
        let result = wait_for_updates(
            Some(1),
            &mut tokio::sync::watch::channel(0).1,
            std::future::pending,
        )
        .await
        .unwrap();
        assert!(result.timed_out);
        assert!(result.updates.is_empty());
        assert_eq!(
            tokio::time::Instant::now() - started,
            Duration::from_secs(1)
        );
    }

    #[tokio::test]
    async fn acknowledgement_keeps_queued_updates_and_those_a_newer_wait_needs() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "queued".into(), None, None, None)
            .await
            .unwrap();
        register_watch(&manager, &workspace.id, "pr").await;
        let delivered = manager
            .record_pr_activity(&workspace.id, vec![("pr".into(), snapshot("review"))])
            .await
            .unwrap();
        let queued = Update {
            url: "pr".into(),
            kind: UpdateKind::BaseMerged,
            message: "base merged".into(),
            delivery: "queued".into(),
            check: None,
            workspace: None,
        };
        let (id, update) = (workspace.id.clone(), queued.clone());
        manager
            .store
            .run(move |db| queue_update(db, &id, update))
            .await
            .unwrap();
        let newer = manager.item_waits.start(&workspace.id);
        manager
            .acknowledge_pr_updates(&workspace.id, vec![delivered[0].delivery.clone()])
            .await
            .unwrap();
        assert_eq!(
            manager.pending_pr_activity(&workspace.id).await.unwrap(),
            [delivered[0].clone(), queued.clone()]
        );
        drop(newer);
        manager
            .acknowledge_pr_updates(&workspace.id, vec![delivered[0].delivery.clone()])
            .await
            .unwrap();
        assert_eq!(
            manager.pending_pr_activity(&workspace.id).await.unwrap(),
            [queued]
        );
    }

    #[tokio::test]
    async fn cursors_survive_restart_are_workspace_owned_and_clear_on_unwatch() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "watch".into(), None, None, None)
            .await
            .unwrap();
        let other = manager
            .create_workspace(&repo.id, "other".into(), None, None, None)
            .await
            .unwrap();
        let changes = || vec![("pr".into(), snapshot("first review"))];
        register_watch(&manager, &workspace.id, "pr").await;
        register_watch(&manager, &other.id, "pr").await;
        let first = manager
            .record_pr_activity(&workspace.id, changes())
            .await
            .unwrap();
        assert_eq!(first.len(), 1);
        let replay = manager
            .record_pr_activity(&workspace.id, changes())
            .await
            .unwrap();
        assert_eq!(replay, first);
        assert_eq!(
            manager
                .record_pr_activity(&other.id, changes())
                .await
                .unwrap()
                .len(),
            1
        );
        let restarted = Manager::open(manager.paths.clone()).await.unwrap();
        assert_eq!(
            restarted.pending_pr_activity(&workspace.id).await.unwrap(),
            first
        );
        restarted
            .acknowledge_pr_updates(&workspace.id, vec!["stale-delivery".into()])
            .await
            .unwrap();
        assert_eq!(
            restarted.pending_pr_activity(&workspace.id).await.unwrap(),
            first
        );
        restarted
            .acknowledge_pr_updates(&workspace.id, vec![first[0].delivery.clone()])
            .await
            .unwrap();
        assert!(
            restarted
                .record_pr_activity(&workspace.id, changes())
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            restarted
                .record_pr_activity(
                    &workspace.id,
                    vec![("pr".into(), snapshot("edited review"))]
                )
                .await
                .unwrap()
                .len(),
            1
        );
        restarted
            .set_pr(&workspace.id, super::super::pr::Action::Clear)
            .await
            .unwrap();
        let id = workspace.id.clone();
        assert_eq!(
            restarted
                .store
                .run(move |db| Ok(db.query_row(
                    "SELECT COUNT(*) FROM pr_activity WHERE workspace_id=?1",
                    [id],
                    |row| row.get::<_, i64>(0)
                )?))
                .await
                .unwrap(),
            0
        );
        assert!(
            restarted
                .record_pr_activity(&workspace.id, changes())
                .await
                .is_err()
        );
        register_watch(&restarted, &workspace.id, "another-pr").await;
        assert!(
            restarted
                .record_pr_activity(&workspace.id, changes())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            restarted
                .pending_pr_activity(&workspace.id)
                .await
                .unwrap()
                .is_empty()
        );
        restarted
            .set_pr(&workspace.id, super::super::pr::Action::Clear)
            .await
            .unwrap();
        assert!(restarted.wait_prs(&workspace.id, Some(0)).await.is_err());
        assert!(restarted.wait_prs(&workspace.id, Some(3601)).await.is_err());
        assert!(
            restarted
                .wait_prs(&workspace.id, Some(3600))
                .await
                .unwrap_err()
                .to_string()
                .contains("no linked items")
        );
        assert!(restarted.completion(&workspace.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn unfiltered_waits_without_a_pr_report_new_branch_conflicts() {
        let (root, manager) = manager().await;
        let repo_path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo_path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "branch".into(), None, None, None)
            .await
            .unwrap();
        let all = Selection::default();

        // A linked PR's forge reports conflicts instead.
        register_watch(&manager, &workspace.id, "pr").await;
        let conflicting = branch_conflicts(Ok(crate::model::ConflictCheck {
            workspace_id: workspace.id.clone(),
            target: "main".into(),
            target_commit: String::new(),
            head: String::new(),
            conflicts: true,
            files: vec!["tracked".into()],
        }));
        let observation = vec![(workspace.branch.clone(), conflicting)];
        let recorded = manager.record_activity(&workspace.id, observation, &all);
        assert!(recorded.await.unwrap().is_empty());
        manager
            .set_pr(&workspace.id, super::super::pr::Action::Clear)
            .await
            .unwrap();

        let poll = || manager.poll_item_activity(&workspace.id, &all);
        assert!(poll().await.unwrap().is_empty());
        for (repo, content) in [(&workspace.path, "workspace\n"), (&repo_path, "main\n")] {
            std::fs::write(repo.join("tracked"), content).unwrap();
            crate::test_support::commit(repo, "tracked");
        }
        let updates = poll().await.unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].url, workspace.branch);
        assert_eq!(updates[0].kind, UpdateKind::MergeConflict);
        assert_eq!(updates[0].message, "Conflicts with main: tracked");
        assert_eq!(
            poll().await.unwrap(),
            updates,
            "unacknowledged updates replay"
        );
        manager
            .acknowledge_pr_updates(&workspace.id, vec![updates[0].delivery.clone()])
            .await
            .unwrap();
        assert!(
            poll().await.unwrap().is_empty(),
            "a persisting conflict is not new"
        );
        let issues = Selection {
            kind: Some(ItemKind::Issue),
            input: None,
        };
        let error = manager.poll_item_activity(&workspace.id, &issues).await;
        assert!(error.unwrap_err().to_string().contains("no linked items"));

        // A failing issue lookup still lets a new branch conflict through.
        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO workspace_issue(workspace_id,url) VALUES (?1,'issue')",
                    [id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let error = poll().await.unwrap_err();
        assert!(error.to_string().contains("origin remote"));
        std::fs::write(repo_path.join("other"), "main\n").unwrap();
        crate::test_support::commit(&repo_path, "other");
        std::fs::write(workspace.path.join("other"), "workspace\n").unwrap();
        crate::test_support::commit(&workspace.path, "other");
        let previous = manager.record_activity(
            &workspace.id,
            vec![(workspace.branch.clone(), resolved_branch(&workspace.id))],
            &all,
        );
        assert!(previous.await.unwrap().is_empty());
        let updates = poll().await.unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].message, "Conflicts with main: other, tracked");
    }

    fn resolved_branch(workspace_id: &str) -> Snapshot {
        branch_conflicts(Ok(crate::model::ConflictCheck {
            workspace_id: workspace_id.into(),
            target: "main".into(),
            target_commit: String::new(),
            head: String::new(),
            conflicts: false,
            files: Vec::new(),
        }))
    }
}
