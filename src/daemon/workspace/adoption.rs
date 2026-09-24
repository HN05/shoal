//! Explicitly transfer a linked worktree into Shoal's normal lifecycle.
use super::{
    Manager, derive_workspace_name, existing_base, ownership, paths::directory_device_inode,
};
use crate::{git, model::Workspace, state::WorkspaceState};
use anyhow::{Context, Result, ensure};
use std::path::Path;
use uuid::Uuid;

impl Manager {
    pub async fn adopt_workspace(&self, repository: &str, path: &Path) -> Result<Workspace> {
        let (repo, _guard) = self.lock_repository(repository).await?;
        let path = self.workspace_location(&repo, path).await?;
        let trees = git::worktrees(&repo.path, git::run).await?;
        let tree = trees
            .iter()
            .find(|tree| tree.path == path)
            .context("path must be the root of a linked worktree of this repository")?;
        ensure!(
            !tree.locked && !tree.prunable,
            "cannot adopt a locked or prunable worktree"
        );
        let branch = tree
            .branch
            .as_deref()
            .and_then(git::strip_local)
            .context("cannot adopt a detached worktree; check out a branch first")?;
        if let Some(workspace) = self
            .list_workspaces()
            .await?
            .into_iter()
            .find(|w| w.path == path)
        {
            ensure!(
                workspace.repository_id == repo.id && workspace.branch == branch,
                "worktree no longer matches its recorded repository or branch"
            );
            ensure!(
                workspace.state == WorkspaceState::Ready,
                "workspace is {}; inspect or set it up before reopening",
                workspace.state
            );
            self.verify_worktree(&workspace).await?;
            self.touch(&workspace.id).await;
            return Ok(workspace);
        }
        let git_dir = ownership::git_dir(&path).await?;
        let identity = directory_device_inode(&git_dir)?;
        let base = existing_base(&repo, branch).await?;
        let commit = git::resolve_commit(&repo.path, &base, git::run_isolated).await?;
        let workspace = Workspace {
            id: Uuid::new_v4().to_string(),
            repository_id: repo.id,
            name: derive_workspace_name(branch),
            path,
            branch: branch.into(),
            state: WorkspaceState::Ready,
            error: None,
            base_commit: Some(commit),
            base_ref: base.starts_with("refs/").then_some(base),
            git_dir: Some(git_dir),
            git_dir_id: Some(identity),
        };
        self.verify_worktree(&workspace).await?;
        // Record identity, base and readiness together: a crash must never leave
        // an adopted directory with weaker ownership checks or pending setup.
        self.insert_workspace(workspace.clone()).await?;
        Ok(workspace)
    }
}
