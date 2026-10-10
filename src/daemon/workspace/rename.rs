//! Reconcile durable rename intent with Git without replaying a ref mutation.
use super::Manager;
use crate::{daemon::store, git, model::Workspace};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

impl Manager {
    pub async fn rename_workspace(
        &self,
        selector: &str,
        branch: &str,
        caller_execution: Option<&str>,
    ) -> Result<Workspace> {
        let _completion = self.pr_gate.lock().await;
        let initial = self.workspace(selector).await?;
        let (repo, _git) = self.lock_repository(&initial.repository_id).await?;
        let workspace = self.workspace(&initial.id).await?;
        let _resources = self
            .resource_guard(
                &workspace.id,
                if caller_execution.is_some() {
                    super::GuardMode::Shared
                } else {
                    super::GuardMode::Exclusive
                },
            )
            .await?;
        self.ensure_no_pending_rename(&workspace.id).await?;
        self.verify_rename_source(&workspace, &repo.path).await?;
        ensure!(
            self.pr_registration(&workspace.id).await?.is_none(),
            "cannot rename a workspace while a PR watch or acknowledgement is recorded"
        );
        git::check_branch_name(Some(&repo.path), branch).await?;
        ensure!(
            !git::worktrunk::is_reserved_branch_name(branch),
            "branch name is incompatible with Worktrunk: {branch}"
        );
        if branch == workspace.branch {
            return Ok(workspace);
        }
        ensure!(
            !git::ref_exists(&repo.path, &git::local_ref(branch), git::isolated_command).await?,
            "local branch already exists: {branch}"
        );
        self.reserve_workspace_rename(&workspace, branch, caller_execution)
            .await?;
        let result = git::run_isolated(
            &workspace.path,
            &["branch", "-m", "--", &workspace.branch, branch],
        )
        .await;
        // Git may have completed even if capturing its output failed. Inspect
        // the owned worktree instead of blindly rolling back a ref mutation.
        if let Err(error) = self.finish_workspace_rename(&workspace, false).await {
            self.set_state(
                &workspace.id,
                crate::state::WorkspaceState::Failed,
                Some(format!(
                    "rename needs repair: {error:#}; run shoal doctor --repair"
                )),
            )
            .await?;
            return Err(error);
        }
        self.set_state(&workspace.id, crate::state::WorkspaceState::Ready, None)
            .await?;
        self.touch(&workspace.id).await;
        result?;
        self.workspace(&workspace.id).await
    }

    async fn verify_rename_source(
        &self,
        workspace: &Workspace,
        checkout: &std::path::Path,
    ) -> Result<()> {
        ensure!(
            workspace.state == crate::state::WorkspaceState::Ready,
            "workspace is not ready"
        );
        self.verify_worktree(workspace).await?;
        let actual = git::head_branch(&workspace.path, true, git::run_isolated).await?;
        ensure!(
            actual.as_deref() == Some(&workspace.branch),
            "worktree is not on its recorded branch"
        );
        let default = git::default_branch::resolve(
            checkout,
            git::default_branch::DefaultBranchLookup::Cached,
        )
        .await?;
        ensure!(
            workspace.branch != default,
            "cannot rename the repository default branch"
        );
        ensure!(
            !git::worktrees(checkout, git::run_isolated)
                .await?
                .iter()
                .any(|tree| tree.is_branch(&workspace.branch) && tree.path != workspace.path),
            "branch is also checked out in another worktree"
        );
        Ok(())
    }

    async fn reserve_workspace_rename(
        &self,
        workspace: &Workspace,
        branch: &str,
        caller_execution: Option<&str>,
    ) -> Result<()> {
        let workspace = workspace.clone();
        let name = crate::validate::workspace_name(branch);
        let branch = branch.to_owned();
        let caller_execution = caller_execution.map(str::to_owned);
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &workspace.id)?;
                ensure!(
                    !store::exists(
                        &tx,
                        "SELECT 1 FROM executions WHERE workspace_id=?1 AND (?2 IS NULL OR id!=?2)",
                        params![workspace.id, caller_execution]
                    )?,
                    "workspace has other active or unknown executions; stop them before renaming"
                );
                ensure!(
                    !store::exists(
                        &tx,
                        "SELECT 1 FROM workspaces WHERE name=?1 AND id!=?2",
                        params![name, workspace.id]
                    )?,
                    "workspace name already exists: {name}"
                );
                ensure!(
                    !store::exists(
                        &tx,
                        "SELECT 1 FROM workspace_renames WHERE name=?1",
                        [&name]
                    )?,
                    "workspace name is reserved by another rename: {name}"
                );
                ensure!(
                    !store::exists(
                        &tx,
                        "SELECT 1 FROM workspaces WHERE repository_id=?1 AND branch=?2 AND id!=?3",
                        params![workspace.repository_id, branch, workspace.id]
                    )?,
                    "branch is already owned by another workspace"
                );
                tx.execute(
                    "INSERT INTO workspace_renames(workspace_id,name,branch) VALUES (?1,?2,?3)",
                    params![workspace.id, name, branch],
                )?;
                tx.execute(
                    "UPDATE workspaces SET state=?2 WHERE id=?1",
                    params![workspace.id, crate::state::WorkspaceState::Reconciling],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub(crate) async fn ensure_no_pending_rename(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                ensure!(
                    !store::exists(
                        db,
                        "SELECT 1 FROM workspace_renames WHERE workspace_id=?1",
                        [&id]
                    )?,
                    "workspace has an unfinished rename; run shoal doctor --repair first"
                );
                Ok(())
            })
            .await
    }

    /// Caller holds the repository Git gate and has reserved the lifecycle.
    /// An unchanged branch cancels intent; a completed Git rename commits it.
    /// Other HEADs and unverifiable ownership retain intent for human repair.
    pub(crate) async fn finish_workspace_rename(
        &self,
        workspace: &Workspace,
        reclaim: bool,
    ) -> Result<bool> {
        let id = workspace.id.clone();
        let pending: Option<(String, String)> = self
            .store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT name,branch FROM workspace_renames WHERE workspace_id=?1",
                        [id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?)
            })
            .await?;
        let Some((name, branch)) = pending else {
            return Ok(false);
        };
        let actual = git::head_branch(&workspace.path, true, git::run_isolated).await?;
        ensure!(
            actual.as_deref() == Some(&workspace.branch) || actual.as_deref() == Some(&branch),
            "unfinished rename expected branch {} or {}; restore one before repair",
            workspace.branch,
            branch
        );
        if let Err(error) = self.verify_worktree(workspace).await {
            ensure!(
                reclaim,
                "unfinished rename ownership cannot be verified: {error:#}; use doctor --repair --reclaim after checking the recorded worktree"
            );
            let candidate = Workspace {
                branch: actual.clone().unwrap(),
                ..workspace.clone()
            };
            self.reclaim_worktree(&candidate).await?;
        }
        let renamed = actual.as_deref() == Some(&branch);
        let workspace = workspace.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                if renamed {
                    tx.execute(
                        "UPDATE workspaces SET name=?2,branch=?3 WHERE id=?1",
                        params![workspace.id, name, branch],
                    )?;
                    tx.execute(
                        "UPDATE workspaces SET base_ref=?3 WHERE repository_id=?1 AND base_ref=?2",
                        params![
                            workspace.repository_id,
                            git::local_ref(&workspace.branch),
                            git::local_ref(&branch)
                        ],
                    )?;
                }
                tx.execute(
                    "DELETE FROM workspace_renames WHERE workspace_id=?1",
                    [workspace.id],
                )?;
                tx.commit()?;
                Ok(true)
            })
            .await
    }
}
