//! Workspace-owned activity cursors; waiting does not change completion policy.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::{RegistrationKind, current_head};
use crate::{
    daemon::{store, workspace::Manager},
    forge::{
        ForgeRepo,
        link::{ItemKind, Selection},
        repository,
        updates::{Snapshot, Update},
    },
    model::Workspace,
};

const POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Serialize, Deserialize)]
pub struct Updates {
    pub updates: Vec<Update>,
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
    /// Run `operation` unless a wait runs in the workspace; no wait can start
    /// while it runs.
    fn unless_running(
        &self,
        workspace: &str,
        operation: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let waits = self.0.lock().expect("active waits lock");
        if waits.contains_key(workspace) {
            return Ok(());
        }
        operation()
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
}

impl Manager {
    #[cfg(test)]
    pub async fn wait_prs(&self, selector: &str, seconds: u64) -> Result<Updates> {
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
        seconds: u64,
    ) -> Result<Updates> {
        ensure!(
            (1..=3600).contains(&seconds),
            "watch timeout must be between 1 and 3600 seconds"
        );
        ensure!(
            selection.input.is_none() || selection.kind.is_some(),
            "explicit items need a kind"
        );
        let workspace = self.workspace(selector).await?;
        let wait = self.item_waits.start(&workspace.id);
        // A cancelled poll leaves its deliveries pending for the newer wait.
        let updates = tokio::select! {
            updates = wait_for_updates(seconds, || {
                self.poll_item_activity(&workspace.id, selection)
            }) => updates?,
            () = wait.superseded.notified() => return Ok(Updates::superseded()),
        };
        if !wait.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(updates)
    }

    async fn poll_item_activity(&self, id: &str, selection: &Selection) -> Result<Vec<Update>> {
        let guard = self.pr_gate.lock().await;
        let workspace = self.workspace(id).await?;
        self.verify_worktree(&workspace).await?;
        current_head(&workspace).await?;
        let items = self.selected_items(&workspace, selection).await?;
        let branch = watches_branch(selection, &items).then(|| workspace.branch.clone());
        let mut keys = items.iter().map(|(url, _)| url.clone()).collect::<Vec<_>>();
        keys.extend(branch.clone());
        let pending = self.pending_activity(id, &keys).await?;
        if !pending.is_empty() {
            return Ok(pending);
        }
        drop(guard);
        let mut observations = self.item_activity(&workspace, items, selection).await?;
        if let Some(branch) = branch {
            let check = self.check_conflicts(id, None).await;
            observations.push((branch, Snapshot::branch_conflicts(check)));
        }
        self.record_activity(id, observations, selection).await
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
        let mut observations = Vec::new();
        for (url, kind) in items {
            let snapshot = match kind {
                ItemKind::Issue => {
                    let (number, _) = forge.issue(&url)?;
                    forge.issue_activity(&workspace.path, number).await?
                }
                ItemKind::Pr => {
                    let (number, _) = forge.pull(&url)?;
                    let branch = selection
                        .input
                        .is_none()
                        .then_some(workspace.branch.as_str());
                    forge.activity(&workspace.path, number, branch).await?
                }
            };
            observations.push((url, snapshot));
        }
        Ok(observations)
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
        if let Some(item) = self.explicit_item(workspace, selection).await? {
            return Ok(vec![item]);
        }
        let id = workspace.id.clone();
        let kind = selection.kind;
        let items = self
            .store
            .run(move |db| linked_items(db, &id, kind))
            .await?;
        ensure!(
            !items.is_empty() || kind.is_none(),
            "no linked items of the selected kind; register one with shoal link"
        );
        Ok(items)
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

    async fn record_activity(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
        selection: &Selection,
    ) -> Result<Vec<Update>> {
        let selection = selection.clone();
        let _guard = self.pr_gate.lock().await;
        let id = id.to_owned();
        self.store.run(move |db| {
            let tx = db.transaction()?;
            store::require_ready(&tx, &id)?;
            let items = linked_items(&tx, &id, selection.kind)?;
            if selection.input.is_none() {
                ensure!(!items.is_empty() || selection.kind.is_none(), "no linked items of the selected kind; register one with shoal link");
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
                let previous: ActivityRecord = previous.map(|text| serde_json::from_str(&text)).transpose()?.unwrap_or_default();
                if !previous.pending.is_empty() {
                    updates.extend(previous.pending);
                    continue;
                }
                snapshot.retain_failed_checks(&previous.snapshot);
                snapshot.schedule_failure_reports(&previous.snapshot, crate::time::unix_seconds());
                let mut pending = previous.snapshot.changes(&snapshot, &url);
                let delivery = uuid::Uuid::new_v4().to_string();
                for update in &mut pending {
                    update.delivery.clone_from(&delivery);
                }
                updates.extend(pending.clone());
                let record = ActivityRecord { snapshot, pending };
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
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &workspace.id)?;
                let records = {
                    let mut statement =
                        tx.prepare("SELECT url, record FROM pr_activity WHERE workspace_id=?1")?;
                    statement
                        .query_map([&workspace.id], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                for (url, record) in records {
                    let mut record: ActivityRecord = serde_json::from_str(&record)?;
                    let pending = record.pending.len();
                    // Updates queued after the acknowledged delivery stay pending.
                    record
                        .pending
                        .retain(|update| !deliveries.contains(&update.delivery));
                    if record.pending.len() != pending {
                        tx.execute(
                            "UPDATE pr_activity SET record=?1 WHERE workspace_id=?2 AND url=?3",
                            params![serde_json::to_string(&record)?, workspace.id, url],
                        )?;
                    }
                }
                // A delivering wait has ended, so a running wait started after
                // it and must report these updates as well.
                waits.unless_running(&workspace.id, || Ok(tx.commit()?))
            })
            .await
    }
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
        if let Some(super::Registration {
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

async fn wait_for_updates<F: std::future::Future<Output = Result<Vec<Update>>>>(
    seconds: u64,
    mut poll: impl FnMut() -> F,
) -> Result<Updates> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        // A cancelled poll may already have committed a pending delivery. The
        // next wait replays it, so the advertised timeout can remain strict.
        let updates = match tokio::time::timeout_at(deadline, poll()).await {
            Ok(updates) => updates?,
            Err(_) => {
                return Ok(Updates {
                    updates: Vec::new(),
                    timed_out: true,
                    superseded: false,
                });
            }
        };
        if !updates.is_empty() {
            return Ok(Updates {
                updates,
                timed_out: false,
                superseded: false,
            });
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(Updates {
                updates: Vec::new(),
                timed_out: true,
                superseded: false,
            });
        }
        tokio::time::sleep(POLL_INTERVAL.min(deadline - now)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        forge::updates::UpdateKind,
        test_support::{manager, repository},
    };
    use serde_json::json;

    #[tokio::test(start_paused = true)]
    async fn polling_wakes_for_a_single_result_and_timeout_has_no_update() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let result = wait_for_updates(3600, || async {
            Ok(
                if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                    Vec::new()
                } else {
                    vec![Update {
                        url: "pr".into(),
                        kind: UpdateKind::CiCompleted,
                        message: "review passed".into(),
                        delivery: String::new(),
                    }]
                },
            )
        })
        .await
        .unwrap();
        assert!(!result.timed_out);
        assert_eq!(result.updates.len(), 1);
        assert_eq!(tokio::time::Instant::now() - started, POLL_INTERVAL);
        let started = tokio::time::Instant::now();
        let result = wait_for_updates(75, || async { Ok(Vec::new()) })
            .await
            .unwrap();
        assert!(result.timed_out && result.updates.is_empty());
        assert_eq!(
            tokio::time::Instant::now() - started,
            Duration::from_secs(75)
        );
        let error = wait_for_updates(3600, || async { anyhow::bail!("lookup failed") })
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "lookup failed");
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

    fn snapshot(message: &str) -> Snapshot {
        serde_json::from_value(json!({"comments": {"one": message}, "checks": {}, "conflict": null, "state": "open", "errors": {}})).unwrap()
    }

    async fn register_watch(manager: &Manager, id: &str, url: &str) {
        let id = id.to_owned();
        let registration = super::super::Registration {
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
            .set_pr(&workspace.id, super::super::Action::Clear)
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
        let result = wait_for_updates(1, || async {
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
        let result = wait_for_updates(1, std::future::pending).await.unwrap();
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
            .set_pr(&workspace.id, super::super::Action::Clear)
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
            .set_pr(&workspace.id, super::super::Action::Clear)
            .await
            .unwrap();
        assert!(restarted.wait_prs(&workspace.id, 0).await.is_err());
        assert!(restarted.wait_prs(&workspace.id, 3601).await.is_err());
        assert!(
            restarted
                .wait_prs(&workspace.id, 3600)
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
        let conflicting = Snapshot::branch_conflicts(Ok(crate::model::ConflictCheck {
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
            .set_pr(&workspace.id, super::super::Action::Clear)
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
    }
}
