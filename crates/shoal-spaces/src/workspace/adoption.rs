//! Adopting a linked worktree into Shoal's lifecycle.
use super::creation::{
    Configured, PendingWorktree, creates_branch_at, existing_base, recorded_base,
};
use crate::Spaces;
use crate::daemon::Daemon;
use crate::{
    fsutil::off_runtime,
    git,
    model::{Base, Checkout, Head, Workspace},
    state::WorkspaceState,
};
use anyhow::{Context, Result, ensure};
use shoal_protocol::space_names::SpaceName;
use shoal_refs::BranchName;
use std::path::{Path, PathBuf};
use tokio::sync::OwnedMutexGuard;

impl Spaces {
    /// With `copy`, creates a new worktree with its files on a branch of its
    /// own instead.
    pub async fn adopt_workspace(
        &self,
        daemon: &dyn Daemon,
        repository: impl Into<crate::repository::Selector>,
        path: &Path,
        copy: bool,
    ) -> Result<Workspace> {
        let (repo, git) = daemon.lock_repository(repository.into()).await?;
        let path = self.workspace_location(daemon, &repo, path).await?;
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
            return self
                .copy_and_adopt(daemon, &repo, (&path, branch), git)
                .await;
        }
        self.adopt_linked_worktree(daemon, &repo, path, &branch.into())
            .await
    }

    async fn adopt_linked_worktree(
        &self,
        daemon: &dyn Daemon,
        repo: &crate::model::Repository,
        path: PathBuf,
        branch: &BranchName,
    ) -> Result<Workspace> {
        if let Some(workspace) = self
            .list_workspaces()
            .await?
            .into_iter()
            .find(|w| w.path == path)
        {
            ensure!(
                workspace.repository() == &repo.id
                    && workspace.branch().map(BranchName::as_str) == Some(branch.as_str()),
                "worktree no longer matches its recorded repository or branch"
            );
            self.reopen_owned_workspace(daemon, &workspace, Some(branch.as_str()))
                .await?;
            return Ok(workspace);
        }
        let git_dir = git::git_dir(&path).await?;
        let base = existing_base(repo, branch).await?;
        let (commit, reference) = recorded_base(repo, &base).await?;
        let mut workspace = Workspace::new_record(
            repo.id.clone(),
            SpaceName::derive(branch.as_str(), "workspace"),
            path,
            Head::Branch(branch.clone()),
            WorkspaceState::Ready,
        );
        workspace.checkout = Checkout::Created {
            base: Base {
                commit: commit.into(),
                reference,
            },
            git_dir: git_dir.clone(),
        };
        // A marker naming no recorded workspace outlived its record or a
        // reset state directory. One naming a recorded workspace refuses
        // adoption.
        self.release_stale_owner(&git_dir, &workspace.id).await?;
        // Mark before the record exists, so no record is ever without its
        // proof. A refused insertion leaves a marker naming no workspace,
        // which the next claim of the directory clears.
        self.record_git_dir(&workspace, git_dir).await?;
        self.verify_worktree(daemon, &workspace).await?;
        // Record the admin directory, base and readiness together: a crash
        // must never leave an adopted directory with pending setup.
        self.insert_workspace(workspace, None, None, None, false)
            .await
    }

    /// Creates the worktree as a pending one's is.
    async fn copy_and_adopt(
        &self,
        daemon: &dyn Daemon,
        repo: &crate::model::Repository,
        (source, source_branch): (&Path, &str),
        git: OwnedMutexGuard<()>,
    ) -> Result<Workspace> {
        let branch = self.available_branch(repo, source_branch).await?;
        // A branch's suffix leaves the name and directory, which take the
        // copied branch's, unsuffixed unless their own are taken.
        let name = SpaceName::derive(source_branch, "workspace");
        let destination = self
            .new_workspace_path(daemon, repo, source_branch, None)
            .await?;
        ensure!(
            !destination.starts_with(source) && !source.starts_with(&destination),
            "copied workspace path overlaps the source worktree"
        );
        let workspace = Workspace::new_record(
            repo.id.clone(),
            name,
            destination,
            Head::Branch(branch),
            WorkspaceState::Preparing,
        );
        // Reserve the branch and destination before Git creates the
        // worktree.
        let pending = PendingWorktree::outside(source.to_owned());
        let workspace = self
            .insert_workspace(workspace, None, None, Some(pending), true)
            .await?;
        self.create_recorded_worktree(daemon, repo, &workspace, git)
            .await
    }

    /// Creates the worktree at the source's HEAD, or verifies the one an
    /// earlier try created, then copies the source's files into it and
    /// records its base. It takes no seed build and needs no setup: the
    /// copied files hold their outputs.
    pub(super) async fn create_outside_copy(
        &self,
        daemon: &dyn Daemon,
        repo: &crate::model::Repository,
        workspace: &Workspace,
        source: &Path,
    ) -> Result<Configured> {
        ensure!(
            off_runtime::exists(source).await?,
            "the worktree {} that {} copies is gone; `shoal rm {}` removes the workspace",
            source.display(),
            workspace.name,
            workspace.number
        );
        let branch = workspace.require_branch("copying a worktree")?;
        match workspace.checkout.git_dir() {
            Some(_) => {
                self.reuse_copied_worktree(daemon, workspace, source)
                    .await?
            }
            None => self.add_copied_worktree(repo, workspace, source).await?,
        }
        crate::fsutil::copy::copy_worktree(source, &workspace.path)
            .context("copy worktree files")?;
        let base = existing_base(repo, branch).await?;
        self.record_worktree_base(repo, workspace, &base).await?;
        Ok(Configured {
            needs_setup: false,
            seed: None,
        })
    }

    /// Waits for a checkpoint to keep what it holds. Refuses a worktree whose
    /// HEAD is no longer the source's, which the files copied now would not
    /// fit.
    async fn reuse_copied_worktree(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        source: &Path,
    ) -> Result<()> {
        self.keep_before_retry(daemon, workspace).await?;
        let (head, started) = tokio::try_join!(
            git::resolve_commit(source, "HEAD", git::run_isolated),
            git::resolve_commit(&workspace.path, "HEAD", git::run_isolated),
        )?;
        ensure!(
            head == started,
            "{} moved from {started}, where {} started, to {head}; `shoal rm {}` removes the workspace, and `shoal adopt --copy` copies it again",
            source.display(),
            workspace.name,
            workspace.number
        );
        Ok(())
    }

    /// Owns the worktree before its files are copied.
    async fn add_copied_worktree(
        &self,
        repo: &crate::model::Repository,
        workspace: &Workspace,
        source: &Path,
    ) -> Result<()> {
        let commit = git::resolve_commit(source, "HEAD", git::run_isolated).await?;
        let new_branch = creates_branch_at(repo, workspace, commit.as_str()).await?;
        git::worktree::add(
            &repo.path,
            &workspace.path,
            workspace.require_branch("copying a worktree")?,
            new_branch.then_some(commit.as_str()),
        )
        .await
        .context("create copied workspace")?;
        // Mark and store the Git admin directory before the possibly long
        // file copy, so a restart can verify and remove the worktree.
        self.record_copy_identity(workspace).await
    }

    async fn record_copy_identity(&self, workspace: &Workspace) -> Result<()> {
        let git_dir = git::git_dir(&workspace.path).await?;
        self.release_stale_owner(&git_dir, &workspace.id).await?;
        self.record_git_dir(workspace, git_dir).await?;
        Ok(())
    }
}
