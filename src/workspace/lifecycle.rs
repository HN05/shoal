//! One removal path for explicit removal and automatic cleanup.
use super::Manager;
use crate::{state::WorkspaceState, worktrunk};
use anyhow::{Result, ensure};
use rusqlite::params;

impl Manager {
    pub async fn check_removal(
        &self,
        selector: String,
        caller_pid: u32,
    ) -> Result<crate::removal::RemovalCheck> {
        let inspection = self.inspect(selector).await?;
        if inspection.workspace.path.exists() {
            self.verify_worktree(&inspection.workspace).await?;
        }
        let repo = self.repository(&inspection.workspace.repository_id).await?;
        let default_branch = crate::default_branch::resolve(&repo.path, false).await.ok();
        crate::removal::check(
            inspection.workspace,
            inspection.executions.len(),
            caller_pid,
            default_branch.as_deref(),
        )
        .await
    }

    pub async fn remove(
        &self,
        selector: String,
        choice: crate::removal::Choice,
        caller_pid: u32,
    ) -> Result<crate::removal::RemovalResult> {
        self.remove_with_guard(selector, choice, caller_pid, None)
            .await
    }

    pub async fn cleanup_snapshot(&self, id: &str) -> Result<Option<u64>> {
        if !self.list_resources(Some(id.into())).await?.is_empty() {
            return Ok(None);
        }
        if self
            .simulators(Some(id.into()))
            .await?
            .iter()
            .any(|s| s.workspace_id.is_some())
        {
            return Ok(None);
        }
        let check = self.check_removal(id.to_owned(), 0).await?;
        if !check.safe() || !check.workspace.path.is_dir() {
            return Ok(None);
        }
        let head = worktrunk::git(&check.workspace.path, &["rev-parse", "HEAD"]).await?;
        let activity = self.activity.lock().await.get(id).copied().unwrap_or(0);
        let path = check.workspace.path;
        Ok(Some(
            tokio::task::spawn_blocking(move || {
                crate::cleanup::fingerprint(&path, &head, activity)
            })
            .await??,
        ))
    }

    pub async fn remove_idle(&self, selector: String, snapshot: u64) -> Result<()> {
        self.remove_with_guard(selector, crate::removal::Choice::Auto, 0, Some(snapshot))
            .await
            .map(|_| ())
    }

    async fn remove_with_guard(
        &self,
        selector: String,
        choice: crate::removal::Choice,
        caller_pid: u32,
        expected_snapshot: Option<u64>,
    ) -> Result<crate::removal::RemovalResult> {
        use crate::removal::{Choice, RemovalResult};
        let automatic = expected_snapshot.is_some();
        let workspace = self.get(selector).await?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                let changed = db.execute(
                    "UPDATE workspaces SET state=?2 WHERE id=?1 AND state IN (?3,?4)",
                    params![
                        id,
                        WorkspaceState::Removing,
                        WorkspaceState::Ready,
                        WorkspaceState::Failed
                    ],
                )?;
                ensure!(changed == 1, "workspace is busy");
                Ok(())
            })
            .await?;
        let result = async {
            let repo = self.repository(&workspace.repository_id).await?;
            let outcome = if workspace.path.exists() {
                let check = self.check_removal(workspace.id.clone(), caller_pid).await?;
                ensure!(!automatic || check.safe(), "workspace is no longer idle, clean and fully pushed");
                ensure!(automatic || !matches!(choice, Choice::Auto) || !check.needs_choice(),
                    "removal requires a branch choice: {}; use --yes with --keep-branch or --delete-branch", check.warnings().join("; "));
                self.stop_executions(&workspace.id, !automatic).await?;
                if let Some(expected) = expected_snapshot {
                    ensure!(
                        self.cleanup_snapshot(&workspace.id).await? == Some(expected),
                        "workspace changed before automatic removal"
                    );
                }
                let check = self.check_removal(workspace.id.clone(), caller_pid).await?;
                ensure!(!automatic || check.safe(), "workspace changed while stopping commands");
                ensure!(automatic || !matches!(choice, Choice::Auto) || !check.needs_choice(),
                    "workspace changed while stopping commands; choose whether to keep or delete the branch");
                let delete_branch = match choice {
                    Choice::Auto => check.can_delete_branch(),
                    Choice::KeepBranch => false,
                    Choice::DeleteBranch => true,
                };
                ensure!(!matches!(choice, Choice::KeepBranch) || check.branch.is_some() || check.unpushed_commits == 0,
                    "detached HEAD has unpushed commits; create a branch before choosing to keep it");
                // Live resources are removed before the directory; failed cleanup
                // retains their ownership records so removal can be retried.
                self.remove_simulators(&workspace.id).await?;
                worktrunk::remove(
                    &repo.path,
                    &self.paths.state.join("worktrunk.toml"),
                    &workspace.path,
                    !matches!(choice, Choice::Auto),
                    delete_branch,
                )
                .await?
            } else {
                ensure!(
                    workspace.state == WorkspaceState::Failed,
                    "workspace directory disappeared; manual reconciliation required"
                );
                ensure!(self.missing_worktree(&workspace).await?.is_none(),
                    "worktree was moved; restore its recorded path before removing it");
                self.stop_executions(&workspace.id, !automatic).await?;
                self.remove_simulators(&workspace.id).await?;
                if self.missing_registration(&workspace).await? {
                    // Prune only this owned registration, through Worktrunk, and
                    // retain its branch because the contents cannot be inspected.
                    worktrunk::remove(&repo.path, &self.paths.state.join("worktrunk.toml"),
                        &workspace.path, true, false).await?
                } else {
                    RemovalResult { removed: true, branch: Some(workspace.branch.clone()), branch_deleted: false, branch_outcome: "retained".into() }
                }
            };
            self.remove_simulators(&workspace.id).await?;
            let id = workspace.id.clone();
            self.store
                .run(move |db| {
                    let tx = db.transaction()?;
                    tx.execute("DELETE FROM executions WHERE workspace_id=?1", [&id])?;
                    tx.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
                    tx.commit()?;
                    Ok(outcome)
                })
                .await
        }
        .await;
        match result {
            Ok(outcome) => {
                self.activity.lock().await.remove(&workspace.id);
                self.scopes
                    .lock()
                    .await
                    .retain(|_, (_, owner)| owner != &workspace.id);
                Ok(outcome)
            }
            Err(error) => {
                self.set_state(&workspace.id, workspace.state, Some(format!("{error:#}")))
                    .await?;
                Err(error)
            }
        }
    }
}
