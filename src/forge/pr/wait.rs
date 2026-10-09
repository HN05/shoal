//! Workspace-owned activity cursors; waiting does not change completion policy.
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

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
        wait_for_updates(seconds, || {
            self.poll_item_activity(&workspace.id, selection)
        })
        .await
    }

    async fn poll_item_activity(&self, id: &str, selection: &Selection) -> Result<Vec<Update>> {
        let guard = self.pr_gate.lock().await;
        let workspace = self.workspace(id).await?;
        self.verify_worktree(&workspace).await?;
        current_head(&workspace).await?;
        let items = self.selected_items(&workspace, selection).await?;
        let urls = items.iter().map(|(url, _)| url.clone()).collect::<Vec<_>>();
        let pending = self.pending_activity(id, &urls).await?;
        if !pending.is_empty() {
            return Ok(pending);
        }
        drop(guard);
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
        self.record_activity(id, observations, selection).await
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

    async fn selected_items(
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
            !items.is_empty(),
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
                ensure!(!items.is_empty(), "no linked items of the selected kind; register one with shoal link");
            }
            let mut updates = Vec::new();
            for (url, mut snapshot) in observations {
                if selection.input.is_none() && !items.iter().any(|(linked, _)| linked == &url) {
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
                tx.commit()?;
                Ok(())
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
                });
            }
        };
        if !updates.is_empty() {
            return Ok(Updates {
                updates,
                timed_out: false,
            });
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(Updates {
                updates: Vec::new(),
                timed_out: true,
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
                .is_err()
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
}
