//! Explicitly transfer a linked worktree into Shoal's normal lifecycle.
use super::{
    Manager, derive_workspace_name, existing_base, identity::directory_identity, ownership,
};
use crate::{
    git::{self, worktrunk},
    model::Workspace,
    state::WorkspaceState,
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

impl Manager {
    pub async fn adopt_workspace(
        &self,
        repository: impl Into<crate::forge::repository::Selector>,
        path: &Path,
    ) -> Result<Workspace> {
        self.adopt_workspace_with_mode(repository, path, false)
            .await
    }

    pub async fn copy_workspace(
        &self,
        repository: impl Into<crate::forge::repository::Selector>,
        path: &Path,
    ) -> Result<Workspace> {
        self.adopt_workspace_with_mode(repository, path, true).await
    }

    async fn adopt_workspace_with_mode(
        &self,
        repository: impl Into<crate::forge::repository::Selector>,
        path: &Path,
        copy: bool,
    ) -> Result<Workspace> {
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
        if copy {
            return self.copy_and_adopt(&repo, &path, branch).await;
        }
        self.adopt_linked_worktree(&repo, path, branch).await
    }

    async fn adopt_linked_worktree(
        &self,
        repo: &crate::model::Repository,
        path: PathBuf,
        branch: &str,
    ) -> Result<Workspace> {
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
        let identity = directory_identity(&git_dir)?;
        let base = existing_base(repo, branch).await?;
        let commit = git::resolve_commit(&repo.path, &base, git::run_isolated).await?;
        let workspace = Workspace {
            base_commit: Some(commit),
            base_ref: base.starts_with("refs/").then_some(base),
            git_dir: Some(git_dir.clone()),
            git_dir_id: Some(identity),
            ..Workspace::new_record(
                repo.id.clone(),
                derive_workspace_name(branch),
                path,
                branch.into(),
                WorkspaceState::Ready,
            )
        };
        // A marker naming no recorded workspace outlived its record or a reset
        // state directory; one naming a recorded workspace refuses adoption.
        self.release_stale_owner(&git_dir, &workspace.id).await?;
        self.verify_worktree(&workspace).await?;
        // Record identity, base and readiness together: a crash must never leave
        // an adopted directory with weaker ownership checks or pending setup.
        self.insert_workspace(workspace.clone()).await?;
        // Mark only once the record exists, so a refused insertion leaves the
        // metadata untouched; the committed identity protects it if marking fails.
        if let Err(error) = self.record_worktree_identity(&workspace).await {
            eprintln!(
                "worktree identity not recorded for {}: {error:#}",
                workspace.name
            );
        }
        Ok(workspace)
    }

    async fn copy_and_adopt(
        &self,
        repo: &crate::model::Repository,
        source: &Path,
        source_branch: &str,
    ) -> Result<Workspace> {
        let branch = self.available_branch(repo, source_branch).await?;
        let name = derive_workspace_name(&branch);
        let destination = self.workspaces_dir(repo).await?.join(&name);
        let destination = self.workspace_location(repo, &destination).await?;
        ensure!(
            !destination.exists(),
            "workspace path already exists: {}",
            destination.display()
        );
        ensure!(
            !destination.starts_with(source) && !source.starts_with(&destination),
            "copied workspace path overlaps the source worktree"
        );
        let commit = git::resolve_commit(source, "HEAD", git::run_isolated).await?;
        worktrunk::create(
            &repo.path,
            &self.paths.worktrunk_config(),
            &destination,
            &branch,
            Some(&commit),
        )
        .await
        .context("create copied workspace")?;
        if let Err(error) = crate::fsutil::copy_worktree(source, &destination) {
            // Leave Git's linked-worktree metadata consistent when copying
            // fails before ownership is recorded.
            let _ = worktrunk::remove(
                &repo.path,
                &self.paths.worktrunk_config(),
                &destination,
                worktrunk::FileRemoval::Force,
                worktrunk::BranchRemoval::Delete,
            )
            .await;
            let _ = fs::remove_dir_all(&destination);
            return Err(error).context("copy worktree files");
        }
        let result = self
            .adopt_linked_worktree(repo, destination.clone(), &branch)
            .await;
        if result.is_err() {
            let _ = worktrunk::remove(
                &repo.path,
                &self.paths.worktrunk_config(),
                &destination,
                worktrunk::FileRemoval::Force,
                worktrunk::BranchRemoval::Delete,
            )
            .await;
        }
        result
    }
}
