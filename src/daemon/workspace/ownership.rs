//! Verify the recorded Git worktree before execution, recovery, or removal.
use super::{
    Manager,
    identity::{directory_identity, mark_owner, marked_owner, verify_owner},
    paths::canonical_parent_only,
};
use crate::{git, model::Workspace};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use std::{
    fs,
    path::{Path, PathBuf},
};

impl Manager {
    pub(crate) async fn verify_worktree(&self, workspace: &Workspace) -> Result<()> {
        let actual_git_dir = self.linked_git_dir(workspace).await?;
        verify_owner(
            &actual_git_dir,
            &workspace.id,
            workspace.git_dir_id.as_deref(),
        )?;
        if let Some(expected) = &workspace.git_dir {
            ensure!(
                fs::canonicalize(expected)? == actual_git_dir,
                "workspace path now refers to a different Git worktree"
            );
        }
        Ok(())
    }

    /// Re-establish ownership of the worktree at the recorded path after a human
    /// confirmed it is this workspace's, whatever made verification fail. It must
    /// still be a linked worktree of the recorded repository on the recorded
    /// branch, and no other workspace may own it.
    pub(crate) async fn reclaim_worktree(&self, workspace: &Workspace) -> Result<()> {
        let git_dir = self.linked_git_dir(workspace).await?;
        let branch = git::head_branch(&workspace.path, true, git::run)
            .await
            .context("worktree HEAD must be on the recorded branch")?;
        ensure!(
            branch.as_deref() == Some(workspace.branch.as_str()),
            "worktree is not on its recorded branch {}; check it out before reclaiming",
            workspace.branch
        );
        let marked = marked_owner(&git_dir)?;
        for other in self.list_workspaces().await? {
            let owns = marked.as_deref() == Some(other.id.as_str())
                || other
                    .git_dir
                    .as_ref()
                    .and_then(|dir| fs::canonicalize(dir).ok())
                    == Some(git_dir.clone());
            ensure!(
                other.id == workspace.id || !owns,
                "Git worktree is owned by workspace {}",
                other.name
            );
        }
        self.record_worktree_identity(workspace).await?;
        self.verify_worktree(&self.workspace(&workspace.id).await?)
            .await
    }

    /// The admin directory of the linked worktree rooted at the recorded path
    /// that belongs to the recorded repository.
    async fn linked_git_dir(&self, workspace: &Workspace) -> Result<PathBuf> {
        let repo = self.repository(&workspace.repository_id).await?;
        // Verify this is still the checkout Shoal created before any deletion.
        let root = git::run(&workspace.path, &["rev-parse", "--show-toplevel"]).await?;
        ensure!(
            fs::canonicalize(root.trim())? == fs::canonicalize(&workspace.path)?,
            "workspace path no longer points to its worktree root"
        );
        let expected = git_common_dir(&repo.path).await?;
        let actual = git_common_dir(&workspace.path).await?;
        ensure!(
            expected == actual,
            "workspace now belongs to a different repository"
        );
        let actual_git_dir = git_dir(&workspace.path).await?;
        ensure!(
            actual_git_dir != actual,
            "workspace was replaced by a main repository checkout"
        );
        Ok(actual_git_dir)
    }

    /// Mark the worktree's admin directory as this workspace's and record its
    /// location and filesystem identity, returning whether anything changed.
    pub(crate) async fn record_worktree_identity(&self, workspace: &Workspace) -> Result<bool> {
        let git_dir = git_dir(&workspace.path).await?;
        let marked = marked_owner(&git_dir)?.as_deref() == Some(workspace.id.as_str());
        if !marked {
            mark_owner(&git_dir, &workspace.id)?;
        }
        // The identity only proves ownership of records made before markers.
        let identity = directory_identity(&git_dir)?;
        if workspace.git_dir.as_ref() == Some(&git_dir)
            && workspace.git_dir_id.as_ref() == Some(&identity)
        {
            return Ok(!marked);
        }
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET git_dir=?2,git_dir_id=?3 WHERE id=?1",
                    params![id, git_dir.to_str(), identity],
                )?;
                Ok(true)
            })
            .await
    }

    /// Whether Git still lists the (possibly missing) worktree at its recorded path.
    pub(crate) async fn is_registered_worktree(&self, workspace: &Workspace) -> Result<bool> {
        let repo = self.repository(&workspace.repository_id).await?;
        let recorded = canonical_parent_only(&workspace.path)?;
        Ok(git::worktrees(&repo.path, git::run)
            .await?
            .iter()
            .any(|tree| tree.path == recorded || tree.path == workspace.path))
    }

    /// Missing worktrees may have been moved outside Shoal, not deleted. Consult
    /// their recorded admin directory before allowing ownership cleanup; only a
    /// link to another existing path needs proof that the directory is ours.
    pub(crate) async fn missing_worktree(&self, workspace: &Workspace) -> Result<Option<PathBuf>> {
        let repo = self.repository(&workspace.repository_id).await?;
        let Some(directory) = &workspace.git_dir else {
            // Older records cannot prove identity after a move; a branch match at
            // another existing path is enough to block forgetting the workspace.
            return Ok(git::worktrees(&repo.path, git::run)
                .await?
                .into_iter()
                .find(|tree| tree.is_branch(&workspace.branch) && tree.path.is_dir())
                .map(|tree| tree.path));
        };
        let destination = match fs::read_to_string(directory.join("gitdir")) {
            Ok(destination) => PathBuf::from(destination.trim_end_matches('\n')),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("read worktree ownership link"),
        };
        let path = destination.parent().context("invalid Git worktree link")?;
        if !path.try_exists()? || fs::canonicalize(path)? == workspace.path {
            return Ok(None);
        }
        // Only this workspace's admin directory proves that its worktree moved;
        // one marked for another workspace means this worktree was deleted.
        match marked_owner(directory)? {
            Some(owner) if owner != workspace.id => return Ok(None),
            _ => verify_owner(directory, &workspace.id, workspace.git_dir_id.as_deref())?,
        }
        Ok(Some(path.to_owned()))
    }
}

async fn git_common_dir(path: &Path) -> Result<PathBuf> {
    let dir = git::run(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    Ok(fs::canonicalize(dir.trim())?)
}

pub(super) async fn git_dir(path: &Path) -> Result<PathBuf> {
    let dir = git::run(path, &["rev-parse", "--path-format=absolute", "--git-dir"]).await?;
    Ok(fs::canonicalize(dir.trim())?)
}
