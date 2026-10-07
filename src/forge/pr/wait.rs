//! Workspace-owned activity cursors; waiting does not change completion policy.
use std::time::Duration;

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{RegistrationKind, current_head};
use crate::{
    daemon::{store, workspace::Manager},
    forge::updates::{Snapshot, Update},
};

const POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Serialize, Deserialize)]
pub struct Updates {
    pub updates: Vec<Update>,
    pub timed_out: bool,
}

impl Manager {
    pub async fn wait_prs(&self, selector: &str, seconds: u64) -> Result<Updates> {
        ensure!(
            (1..=3600).contains(&seconds),
            "PR wait timeout must be between 1 and 3600 seconds"
        );
        let workspace = self.workspace(selector).await?;
        wait_for_updates(seconds, || self.poll_pr_activity(&workspace.id)).await
    }

    async fn poll_pr_activity(&self, id: &str) -> Result<Vec<Update>> {
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(id).await?;
        self.verify_worktree(&workspace).await?;
        current_head(&workspace).await?;
        let Some(registration) = self.pr_registration(id).await? else {
            anyhow::bail!("no watched PRs; register one with shoal pr watch");
        };
        let RegistrationKind::Watch { urls, .. } = registration.kind else {
            anyhow::bail!("no watched PRs; register one with shoal pr watch");
        };
        let mut observations = Vec::new();
        for url in urls {
            let (forge, number, _) = self.pr_forge(&workspace, &url).await?;
            observations.push((
                url,
                forge
                    .activity(&workspace.path, number, &workspace.branch)
                    .await?,
            ));
        }
        self.record_pr_activity(id, observations).await
    }

    async fn record_pr_activity(
        &self,
        id: &str,
        observations: Vec<(String, Snapshot)>,
    ) -> Result<Vec<Update>> {
        let id = id.to_owned();
        self.store.run(move |db| {
            let tx = db.transaction()?;
            store::require_ready(&tx, &id)?;
            let mut updates = Vec::new();
            for (url, mut snapshot) in observations {
                let previous: Option<String> = tx.query_row(
                    "SELECT record FROM pr_activity WHERE workspace_id=?1 AND url=?2",
                    params![id, url], |row| row.get(0),
                ).optional()?;
                let previous: Snapshot = previous.map(|text| serde_json::from_str(&text)).transpose()?.unwrap_or_default();
                snapshot.retain_failed_checks(&previous);
                updates.extend(previous.changes(&snapshot, &url));
                tx.execute("INSERT INTO pr_activity(workspace_id,url,record) VALUES (?1,?2,?3) ON CONFLICT(workspace_id,url) DO UPDATE SET record=excluded.record", params![id, url, serde_json::to_string(&snapshot)?])?;
            }
            ensure!(serde_json::to_vec(&updates)?.len() < crate::protocol::MAX_FRAME - 256,
                "too many PR updates for one response; cancel unused watches");
            tx.commit()?;
            Ok(updates)
        }).await
    }
}

async fn wait_for_updates<F: std::future::Future<Output = Result<Vec<Update>>>>(
    seconds: u64,
    mut poll: impl FnMut() -> F,
) -> Result<Updates> {
    let wait = async {
        loop {
            let updates = poll().await?;
            if !updates.is_empty() {
                return Ok::<_, anyhow::Error>(updates);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    };
    match tokio::time::timeout(Duration::from_secs(seconds), wait).await {
        Ok(updates) => Ok(Updates {
            updates: updates?,
            timed_out: false,
        }),
        Err(_) => Ok(Updates {
            updates: Vec::new(),
            timed_out: true,
        }),
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
        assert_eq!(
            manager
                .record_pr_activity(&workspace.id, changes())
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            manager
                .record_pr_activity(&workspace.id, changes())
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            manager
                .record_pr_activity(&other.id, changes())
                .await
                .unwrap()
                .len(),
            1
        );
        let restarted = Manager::open(manager.paths.clone()).await.unwrap();
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
        assert_eq!(
            restarted
                .record_pr_activity(&workspace.id, changes())
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(restarted.wait_prs(&workspace.id, 0).await.is_err());
        assert!(restarted.wait_prs(&workspace.id, 3601).await.is_err());
        assert!(
            restarted
                .wait_prs(&workspace.id, 3600)
                .await
                .unwrap_err()
                .to_string()
                .contains("no watched PRs")
        );
        assert!(restarted.completion(&workspace.id).await.unwrap().is_none());
    }
}
