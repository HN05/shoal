//! Verify the recorded Git worktree before execution, recovery, or removal.
use super::Manager;
use crate::{git, model::Workspace};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use std::{
    fs,
    path::{Path, PathBuf},
};

impl Manager {
    pub(crate) async fn verify_worktree(&self, workspace: &Workspace) -> Result<()> {
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
        if let Some(identity) = &workspace.git_dir_id {
            ensure!(
                directory_identity(&actual_git_dir)? == *identity,
                "Git worktree metadata was replaced; ownership cannot be verified"
            );
        }
        if let Some(expected) = &workspace.git_dir {
            ensure!(
                fs::canonicalize(expected)? == actual_git_dir,
                "workspace path now refers to a different Git worktree"
            );
        }
        Ok(())
    }

    pub(crate) async fn record_worktree_identity(&self, workspace: &Workspace) -> Result<()> {
        let git_dir = git_dir(&workspace.path).await?;
        let identity = directory_identity(&git_dir)?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET git_dir=?2,git_dir_id=?3 WHERE id=?1",
                    params![id, git_dir.to_str(), identity],
                )?;
                Ok(())
            })
            .await
    }

    /// Whether Git still lists the (possibly missing) worktree at its recorded path.
    pub(crate) async fn is_registered_worktree(&self, workspace: &Workspace) -> Result<bool> {
        let repo = self.repository(&workspace.repository_id).await?;
        let recorded = canonical_parent(&workspace.path)?;
        Ok(git::worktrees(&repo.path)
            .await?
            .iter()
            .any(|tree| tree.path == recorded || tree.path == workspace.path))
    }

    /// Missing worktrees may have been moved outside Shoal, not deleted. Consult
    /// their recorded admin directory before allowing ownership cleanup.
    pub(crate) async fn missing_worktree(&self, workspace: &Workspace) -> Result<Option<PathBuf>> {
        let repo = self.repository(&workspace.repository_id).await?;
        let Some(directory) = &workspace.git_dir else {
            // Older records cannot prove identity after a move; a branch match at
            // another existing path is enough to block forgetting the workspace.
            return Ok(git::worktrees(&repo.path)
                .await?
                .into_iter()
                .find(|tree| tree.is_branch(&workspace.branch) && tree.path.is_dir())
                .map(|tree| tree.path));
        };
        if directory.try_exists()? {
            if let Some(identity) = &workspace.git_dir_id {
                ensure!(
                    directory_identity(directory)? == *identity,
                    "Git worktree metadata was replaced; ownership cannot be verified"
                );
            }
        }
        match fs::read_to_string(directory.join("gitdir")) {
            Ok(destination) => {
                let path = PathBuf::from(destination.trim_end_matches('\n'));
                let path = path.parent().context("invalid Git worktree link")?;
                if path.try_exists()? && fs::canonicalize(path)? != workspace.path {
                    return Ok(Some(path.to_owned()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("read worktree ownership link"),
        }
        Ok(None)
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

/// The path with its parent canonicalized, so a missing leaf still compares
/// against Git's absolute worktree records.
pub(super) fn canonical_parent(path: &Path) -> Result<PathBuf> {
    let parent = fs::canonicalize(path.parent().context("missing workspace parent")?)?;
    Ok(parent.join(path.file_name().context("missing workspace name")?))
}

/// `device:inode` of a directory, stable across renames but not replacement.
pub(super) fn device_inode(metadata: &fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}:{}", metadata.dev(), metadata.ino())
}

pub(super) fn directory_identity(path: &Path) -> Result<String> {
    let metadata = fs::metadata(path)?;
    ensure!(metadata.is_dir(), "Git metadata is not a directory");
    Ok(device_inode(&metadata))
}
