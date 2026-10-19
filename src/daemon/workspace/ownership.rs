//! Verify the recorded Git worktree before execution, recovery, or removal.
use super::{
    Manager,
    identity::{
        clear_owner, directory_identity, has_directory_identity, mark_owner, marked_owner,
        verify_owner,
    },
    paths::matches_recorded_worktree_path,
};
use crate::{git, model::Workspace, state::WorkspaceState};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Whether a recorded workspace owns a Git administrative directory.
///
/// A marker is authoritative for workspaces created with marker support. The
/// recorded path and filesystem identity cover records from before markers,
/// and are both compared so path aliases and identity upgrades use the same
/// collision rule everywhere a worktree is claimed.
pub(super) fn owns_git_dir(
    workspace: &Workspace,
    git_dir: &Path,
    identity: Option<&str>,
    marker: Option<&str>,
) -> Result<bool> {
    let canonical_git_dir = fs::canonicalize(git_dir).ok();
    let same_path = workspace.git_dir.as_ref().is_some_and(|recorded| {
        match (canonical_git_dir.as_ref(), fs::canonicalize(recorded).ok()) {
            (Some(canonical_git_dir), Some(canonical_recorded)) => {
                canonical_recorded == *canonical_git_dir
            }
            _ => recorded == git_dir,
        }
    });
    if marker == Some(workspace.id.as_str()) || same_path {
        return Ok(true);
    }
    let same_identity = match (identity, workspace.git_dir_id.as_deref()) {
        (Some(identity), Some(recorded)) => {
            identity == recorded || has_directory_identity(git_dir, recorded)?
        }
        _ => false,
    };
    Ok(same_identity)
}

impl Manager {
    /// Validate and reactivate an owned workspace being reopened by branch or path.
    pub(crate) async fn reopen_owned_workspace(
        &self,
        workspace: &Workspace,
        expected_branch: &str,
    ) -> Result<()> {
        ensure!(
            workspace.state == WorkspaceState::Ready,
            "workspace {} is {}; inspect or set it up before reopening",
            workspace.name,
            workspace.state
        );
        self.verify_worktree(workspace).await?;
        let actual = git::head_branch(&workspace.path, false, git::run_isolated).await?;
        ensure!(
            actual.as_deref() == Some(expected_branch),
            "workspace {} is no longer on its recorded branch",
            workspace.name
        );
        self.touch(&workspace.id).await;
        Ok(())
    }

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
        self.release_stale_owner(&git_dir, &workspace.id).await?;
        self.record_worktree_identity(workspace).await?;
        self.verify_worktree(&self.workspace(&workspace.id).await?)
            .await
    }

    /// Refuse an admin directory another recorded workspace owns, by marker or
    /// recorded location, and clear a marker naming no recorded workspace: one
    /// left behind when a record was forgotten or the state directory reset.
    pub(super) async fn release_stale_owner(
        &self,
        git_dir: &Path,
        workspace_id: &str,
    ) -> Result<()> {
        let marked = marked_owner(git_dir)?;
        let identity = directory_identity(git_dir)?;
        for other in self.list_workspaces().await? {
            let owns = owns_git_dir(&other, git_dir, Some(&identity), marked.as_deref())?;
            ensure!(
                other.id == workspace_id || !owns,
                "Git worktree is already owned by {}; restore its recorded path",
                other.name
            );
        }
        if marked.is_some_and(|owner| owner != workspace_id) {
            clear_owner(git_dir)?;
        }
        Ok(())
    }

    /// The admin directory of the linked worktree rooted at the recorded path
    /// that belongs to the recorded repository.
    async fn linked_git_dir(&self, workspace: &Workspace) -> Result<PathBuf> {
        let repo = self.repository(&workspace.repository_id).await?;
        // Verify this is still the checkout Shoal created before any deletion.
        let root = git::worktree_root(&workspace.path).await?;
        ensure!(
            fs::canonicalize(root.trim())? == fs::canonicalize(&workspace.path)?,
            "workspace path no longer points to its worktree root"
        );
        let expected = git::common_dir(&repo.path).await?;
        let actual = git::common_dir(&workspace.path).await?;
        ensure!(
            expected == actual,
            "workspace now belongs to a different repository"
        );
        let actual_git_dir = git::git_dir(&workspace.path).await?;
        ensure!(
            actual_git_dir != actual,
            "workspace was replaced by a main repository checkout"
        );
        Ok(actual_git_dir)
    }

    /// Mark the worktree's admin directory as this workspace's and record its
    /// location and filesystem identity, returning whether anything changed.
    pub(crate) async fn record_worktree_identity(&self, workspace: &Workspace) -> Result<bool> {
        let git_dir = git::git_dir(&workspace.path).await?;
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
        for tree in git::worktrees(&repo.path, git::run).await? {
            if matches_recorded_worktree_path(&workspace.path, &tree.path)? {
                return Ok(true);
            }
        }
        Ok(false)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::WorkspaceState;

    fn workspace() -> Workspace {
        Workspace::new_record(
            "repository".into(),
            "workspace".into(),
            PathBuf::from("/workspace"),
            "branch".into(),
            WorkspaceState::Ready,
        )
    }

    #[test]
    fn ownership_uses_the_same_marker_path_and_identity_evidence() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let git_dir = temp.path().join("admin");
        fs::create_dir(&git_dir)?;
        let different = temp.path().join("different");
        fs::create_dir(&different)?;
        let mut other = workspace();
        other.git_dir = Some(temp.path().join("admin/../admin"));
        other.git_dir_id = Some("old-identity".into());

        assert!(owns_git_dir(&other, &git_dir, Some("new-identity"), None)?);
        assert!(owns_git_dir(
            &other,
            &different,
            Some("old-identity"),
            None
        )?);
        assert!(owns_git_dir(
            &other,
            Path::new("/missing"),
            None,
            Some(&other.id)
        )?);
        assert!(!owns_git_dir(
            &other,
            &different,
            Some("new-identity"),
            None
        )?);
        Ok(())
    }

    #[test]
    fn legacy_identity_matches_the_current_birth_identity() -> Result<()> {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir()?;
        let git_dir = temp.path().join("admin");
        let other_dir = temp.path().join("other");
        fs::create_dir(&git_dir)?;
        fs::create_dir(&other_dir)?;
        let metadata = fs::metadata(&git_dir)?;
        let mut other = workspace();
        other.git_dir = Some(other_dir);
        other.git_dir_id = Some(format!("{}:{}", metadata.dev(), metadata.ino()));

        assert!(owns_git_dir(
            &other,
            &git_dir,
            Some(&directory_identity(&git_dir)?),
            None
        )?);
        Ok(())
    }
}
