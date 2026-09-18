//! Persisted opt-in PR watches and manual merge acknowledgements.
use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::{
    forge::ForgeRepo, git, model::Workspace, notifications::NotificationKind, repository, store,
    workspace::Manager,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registration {
    pub url: Option<String>,
    /// Manual acknowledgement is tied to exactly this commit.
    pub head: Option<String>,
    pub error: Option<String>,
}

impl Manager {
    pub async fn pr_registration(&self, id: &str) -> Result<Option<Registration>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let record: Option<String> = db
                    .query_row(
                        "SELECT record FROM pr_cleanup WHERE workspace_id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?;
                record
                    .map(|s| serde_json::from_str(&s).map_err(Into::into))
                    .transpose()
            })
            .await
    }

    pub async fn set_pr(&self, selector: &str, url: Option<String>, clear: bool) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        ensure!(
            clear || self.config.pr_cleanup.enabled,
            "PR cleanup is disabled by [pr_cleanup] enabled = false"
        );
        let registration = if clear {
            None
        } else {
            self.verify_worktree(&workspace).await?;
            let head = current_head(&workspace).await?;
            if let Some(url) = &url {
                let (forge, number) = self.pr_forge(&workspace, url).await?;
                // Detect missing tools/login, wrong branches and invalid URLs now.
                forge
                    .merged_commits(&workspace.path, number, &workspace.branch)
                    .await?;
            }
            Some(Registration {
                head: url.is_none().then_some(head),
                url,
                error: None,
            })
        };
        let id = workspace.id;
        self.store.run(move |db| {
            let tx = db.transaction()?;
            store::require_ready(&tx, &id)?;
            if let Some(registration) = registration {
                tx.execute("INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2) ON CONFLICT(workspace_id) DO UPDATE SET record=excluded.record", rusqlite::params![id, serde_json::to_string(&registration)?])?;
            } else { tx.execute("DELETE FROM pr_cleanup WHERE workspace_id=?1", [&id])?; }
            tx.commit()?;
            Ok(())
        }).await?;
        self.cleanup_notify.notify_one();
        Ok(())
    }

    async fn pr_forge(&self, workspace: &Workspace, url: &str) -> Result<(ForgeRepo, u64)> {
        let remote = repository::remote_url(
            workspace
                .path
                .to_str()
                .context("workspace path is not UTF-8")?,
        )
        .await?
        .context("PR lookup needs an origin remote")?;
        let forge = ForgeRepo::parse(&remote)?;
        let number = forge.pull(url)?;
        Ok((forge, number))
    }

    /// Watches survive restarts. A failed lookup never counts as a merge and a
    /// failed removal keeps its registration and ownership records for retry.
    pub async fn sweep_prs(&self) -> Result<()> {
        if !self.config.pr_cleanup.enabled {
            return Ok(());
        }
        let _guard = self.pr_gate.lock().await;
        for workspace in self.list_workspaces().await? {
            if workspace.state != crate::state::WorkspaceState::Ready {
                continue;
            }
            let Some(mut registration) = self.pr_registration(&workspace.id).await? else {
                continue;
            };
            // `Ok(true)` once the workspace is removed; `Ok(false)` while the PR is open.
            let result: Result<bool> = async {
                self.verify_worktree(&workspace).await?;
                let head = current_head(&workspace).await?;
                if let Some(url) = &registration.url {
                    let (forge, number) = self.pr_forge(&workspace, url).await?;
                    let Some(commits) = forge.merged_commits(&workspace.path, number, &workspace.branch).await? else { return Ok(false); };
                    ensure!(commits.contains(&head), "merged PR does not contain the current workspace commit; retaining workspace");
                } else {
                    ensure!(registration.head.as_deref() == Some(&head), "HEAD changed after the merge acknowledgement; retaining workspace");
                }
                self.remove_merged(&workspace.id, &head).await?;
                eprintln!("PR cleanup removed {}", workspace.name);
                Ok(true)
            }.await;
            match &result {
                Ok(true) => {
                    let cause = if registration.url.is_some() {
                        "removed after its pull request merged"
                    } else {
                        "removed after the merge acknowledgement"
                    };
                    self.notify(
                        Some(&workspace.name),
                        NotificationKind::WorkspaceRemoved,
                        cause,
                    )
                    .await;
                }
                Ok(false) => {}
                Err(error) => {
                    self.notify(
                        Some(&workspace.name),
                        NotificationKind::CleanupFailed,
                        format!("PR cleanup retained the workspace: {error:#}"),
                    )
                    .await;
                }
            }
            registration.error = result.err().map(|error| format!("{error:#}"));
            let id = workspace.id;
            self.store
                .run(move |db| {
                    db.execute(
                        "UPDATE pr_cleanup SET record=?2 WHERE workspace_id=?1",
                        rusqlite::params![id, serde_json::to_string(&registration)?],
                    )?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }
}

pub(crate) async fn current_head(workspace: &Workspace) -> Result<String> {
    ensure!(
        git::run(&workspace.path, &["branch", "--show-current"])
            .await?
            .trim_end()
            == workspace.branch,
        "workspace is not on its recorded branch"
    );
    Ok(git::run(&workspace.path, &["rev-parse", "HEAD"])
        .await?
        .trim()
        .to_owned())
}
