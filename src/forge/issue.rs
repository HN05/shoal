//! Persist the issue selected when opening a workspace.
use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::{ForgeRepo, repository};
use crate::{daemon::workspace::Manager, model::Workspace, state::WorkspaceState};

#[derive(Debug, Serialize, Deserialize)]
pub struct Registration {
    pub url: String,
    pub error: Option<String>,
}

impl Manager {
    pub async fn issue_registration(&self, id: &str) -> Result<Option<Registration>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT url,error FROM workspace_issue WHERE workspace_id=?1",
                        [id],
                        |row| {
                            Ok(Registration {
                                url: row.get(0)?,
                                error: row.get(1)?,
                            })
                        },
                    )
                    .optional()?)
            })
            .await
    }

    /// Association is part of opening a workspace, before tracked setup starts.
    pub async fn set_issue(&self, selector: &str, input: &str) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let (_, _, url) = self.issue_forge(&workspace, input).await?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let state: WorkspaceState = tx.query_row(
                    "SELECT state FROM workspaces WHERE id=?1",
                    [&workspace.id],
                    |row| row.get(0),
                )?;
                ensure!(
                    matches!(
                        state,
                        WorkspaceState::Ready | WorkspaceState::Preparing | WorkspaceState::Failed
                    ),
                    "workspace cannot accept an issue in its current state"
                );
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT url FROM workspace_issue WHERE workspace_id=?1",
                        [&workspace.id],
                        |row| row.get(0),
                    )
                    .optional()?;
                ensure!(
                    existing.as_ref().is_none_or(|existing| existing == &url),
                    "workspace is already associated with a different issue"
                );
                tx.execute(
                    "INSERT INTO workspace_issue(workspace_id,url) VALUES (?1,?2)
                 ON CONFLICT(workspace_id) DO NOTHING",
                    rusqlite::params![workspace.id, url],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    async fn issue_forge(
        &self,
        workspace: &Workspace,
        input: &str,
    ) -> Result<(ForgeRepo, u64, String)> {
        let remote = repository::remote_url_from_path(&workspace.path)
            .await?
            .context("issue lookup needs an origin remote")?;
        let forge = ForgeRepo::parse(&remote)?;
        let (number, url) = forge.issue(input)?;
        Ok((forge, number, url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git, manager, repository};

    #[tokio::test]
    async fn association_is_persistent_idempotent_and_bound_to_the_origin() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        git(
            &path,
            &["remote", "add", "origin", "https://github.com/team/repo"],
        );
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "issue".into(), Some("HEAD".into()), None, None)
            .await
            .unwrap();
        let url = "https://github.com/team/repo/issues/316";
        manager.set_issue(&workspace.id, url).await.unwrap();
        manager.set_issue(&workspace.id, "316").await.unwrap();
        assert!(manager.set_issue(&workspace.id, "317").await.is_err());
        assert!(
            manager
                .set_issue(&workspace.id, "https://github.com/other/repo/issues/316")
                .await
                .is_err()
        );
        let reopened = Manager::open(manager.paths.clone()).await.unwrap();
        assert_eq!(
            reopened
                .inspect_workspace(&workspace.id)
                .await
                .unwrap()
                .issue
                .unwrap()
                .url,
            url
        );
        reopened.store.shutdown().await;
    }
}
