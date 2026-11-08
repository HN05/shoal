//! Verify the recorded Git worktree before execution, recovery, or removal.
use super::{
    Manager,
    identity::{
        clear_owner, directory_identity, has_directory_identity, mark_owner, marked_owner,
        verify_owner,
    },
    paths::matches_recorded_worktree_path,
};
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::refusal::{Reason, Refusal};
use crate::{fsutil::off_runtime, git, model::Workspace, state::WorkspaceState};
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

/// A worktree whose ownership verified.
pub(crate) struct VerifiedWorktree {
    /// Whether recording the identity changed it. A failure leaves the existing
    /// record, which verification just accepted, protecting the worktree.
    pub identity: Result<bool>,
}

impl Manager {
    /// Validate and reactivate an owned workspace being reopened by branch or path.
    /// Reopen the recorded workspace `selector` for a launch in it; a removed
    /// workspace fails rather than being created again.
    pub async fn reopen_workspace(
        &self,
        selector: &SpaceSelector,
    ) -> Result<crate::model::OpenedWorkspace> {
        let workspace = self.workspace(selector).await?;
        let (_repo, _guard) = self.lock_repository(&workspace.repository_id).await?;
        // The workspace may have gone while waiting for the repository.
        let workspace = self.workspace_by_id(&workspace.id).await?;
        self.reopen_owned_workspace(&workspace, workspace.branch.as_deref())
            .await?;
        Ok(crate::model::OpenedWorkspace {
            workspace,
            reused: true,
        })
    }

    pub(crate) async fn reopen_owned_workspace(
        &self,
        workspace: &Workspace,
        expected_branch: Option<&str>,
    ) -> Result<()> {
        self.check_reopen(workspace, expected_branch).await?;
        self.touch(&workspace.id).await;
        Ok(())
    }

    /// Refuse reopening a workspace that is not ready, no longer its verified
    /// worktree, or off `expected_branch`, without touching it.
    pub(crate) async fn check_reopen(
        &self,
        workspace: &Workspace,
        expected_branch: Option<&str>,
    ) -> Result<()> {
        ensure!(
            workspace.state == WorkspaceState::Ready,
            "workspace {} is {}; inspect or set it up before reopening",
            workspace.name,
            workspace.state
        );
        self.verify_checkout(workspace, expected_branch).await
    }

    /// Refuse a worktree that is no longer the recorded one or is off
    /// `expected_branch`; `None` expects a detached HEAD.
    pub(crate) async fn verify_checkout(
        &self,
        workspace: &Workspace,
        expected_branch: Option<&str>,
    ) -> Result<()> {
        self.verify_worktree(workspace).await?;
        let Some(expected_branch) = expected_branch else {
            let (_, branch) = git::head_and_branch(&workspace.path).await?;
            ensure!(
                branch.is_none(),
                "workspace {} is at a commit but its worktree is on a branch",
                workspace.name
            );
            return Ok(());
        };
        let actual = git::head_branch(&workspace.path, false, git::run_isolated).await?;
        ensure!(
            actual.as_deref() == Some(expected_branch),
            "workspace {} is no longer on its recorded branch",
            workspace.name
        );
        Ok(())
    }

    /// The workspace `selector` names once its worktree verifies, and with
    /// `on_branch` once it is on its recorded branch.
    pub(crate) async fn verify_workspace(
        &self,
        selector: &SpaceSelector,
        on_branch: bool,
    ) -> Result<Workspace> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        if on_branch {
            crate::daemon::links::pr::current_head(&workspace).await?;
        }
        Ok(workspace)
    }

    pub(crate) async fn verify_worktree(&self, workspace: &Workspace) -> Result<()> {
        self.verify_worktree_identity(workspace)
            .await
            .map_err(|error| Refusal::wrap(Reason::NotReady, error))
    }

    async fn verify_worktree_identity(&self, workspace: &Workspace) -> Result<()> {
        let actual_git_dir = self.linked_git_dir(workspace).await?;
        let workspace = workspace.clone();
        off_runtime::run(move || verify_git_dir(&workspace, &actual_git_dir)).await
    }

    /// Verify ownership, then record the worktree's identity when `record` is
    /// set. Recording runs only after verification passes; an error from
    /// verification means the worktree is not proven to be this workspace's.
    pub(crate) async fn verify_and_record_worktree(
        &self,
        workspace: &Workspace,
        record: bool,
    ) -> Result<VerifiedWorktree> {
        self.verify_worktree(workspace).await?;
        let identity = if record {
            self.record_worktree_identity(workspace).await
        } else {
            Ok(false)
        };
        Ok(VerifiedWorktree { identity })
    }

    /// Re-establish ownership of the worktree at the recorded path after a human
    /// confirmed it is this workspace's, whatever made verification fail. It must
    /// still be a linked worktree of the recorded repository on the recorded
    /// branch, and no other workspace may own it.
    pub(crate) async fn reclaim_worktree(&self, workspace: &Workspace) -> Result<()> {
        let git_dir = self.linked_git_dir(workspace).await?;
        match &workspace.branch {
            Some(recorded) => {
                let branch = git::head_branch(&workspace.path, true, git::run)
                    .await
                    .context("worktree HEAD must be on the recorded branch")?;
                ensure!(
                    branch.as_deref() == Some(recorded.as_str()),
                    "worktree is not on its recorded branch {recorded}; check it out before reclaiming"
                );
            }
            None => {
                let (_, branch) = git::head_and_branch(&workspace.path).await?;
                ensure!(
                    branch.is_none(),
                    "worktree is on branch {}, but its workspace is at a commit; detach it before reclaiming",
                    branch.unwrap_or_default()
                );
            }
        }
        self.release_stale_owner(&git_dir, &workspace.id).await?;
        self.record_worktree_identity(workspace).await?;
        self.verify_worktree(&self.workspace_by_id(&workspace.id).await?)
            .await
    }

    /// Refuse an admin directory another recorded workspace owns, by marker or
    /// recorded location, and clear a marker naming no recorded workspace: one
    /// left behind when a record was forgotten or the state directory reset.
    pub(super) async fn release_stale_owner(
        &self,
        git_dir: &Path,
        workspace_id: &SpaceId,
    ) -> Result<()> {
        let workspaces = self.list_workspaces().await?;
        let git_dir = git_dir.to_owned();
        let workspace_id = workspace_id.to_owned();
        off_runtime::run(move || release_stale_marker(&git_dir, &workspace_id, &workspaces)).await
    }

    /// The admin directory of the linked worktree rooted at the recorded path
    /// that belongs to the recorded repository.
    async fn linked_git_dir(&self, workspace: &Workspace) -> Result<PathBuf> {
        let repo = self.repository(&workspace.repository_id).await?;
        // Verify this is still the checkout Shoal created before any deletion.
        let root = git::worktree_root(&workspace.path).await?;
        ensure!(
            off_runtime::canonicalize(Path::new(root.trim())).await?
                == off_runtime::canonicalize(&workspace.path).await?,
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
        let (marked, identity) = {
            let (git_dir, id) = (git_dir.clone(), workspace.id.clone());
            off_runtime::run(move || mark_git_dir(&git_dir, &id)).await?
        };
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
        let trees = git::worktrees(&repo.path, git::run).await?;
        let recorded = workspace.path.clone();
        off_runtime::run(move || {
            for tree in trees {
                if matches_recorded_worktree_path(&recorded, &tree.path)? {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .await
    }

    /// Missing worktrees may have been moved outside Shoal, not deleted. Consult
    /// their recorded admin directory before allowing ownership cleanup; only a
    /// link to another existing path needs proof that the directory is ours.
    pub(crate) async fn missing_worktree(&self, workspace: &Workspace) -> Result<Option<PathBuf>> {
        let repo = self.repository(&workspace.repository_id).await?;
        if let Some(directory) = workspace.git_dir.clone() {
            let workspace = workspace.clone();
            return off_runtime::run(move || moved_worktree(&workspace, &directory)).await;
        }
        // Older records cannot prove identity after a move; a branch match at
        // another existing path is enough to block forgetting the workspace.
        // Every workspace without a branch records its Git directory.
        let Some(branch) = workspace.branch.clone() else {
            return Ok(None);
        };
        let trees = git::worktrees(&repo.path, git::run).await?;
        off_runtime::run(move || {
            Ok(trees
                .into_iter()
                .find(|tree| tree.is_branch(&branch) && tree.path.is_dir())
                .map(|tree| tree.path))
        })
        .await
    }
}

/// Refuse an admin directory that is not the workspace's own, or not the one
/// it recorded.
fn verify_git_dir(workspace: &Workspace, git_dir: &Path) -> Result<()> {
    verify_owner(git_dir, &workspace.id, workspace.git_dir_id.as_deref())?;
    if let Some(expected) = &workspace.git_dir {
        ensure!(
            fs::canonicalize(expected)? == git_dir,
            "workspace path now refers to a different Git worktree"
        );
    }
    Ok(())
}

/// Refuse an admin directory another of the `workspaces` owns, and clear a
/// marker naming a workspace other than `workspace_id`, which none of them is.
fn release_stale_marker(
    git_dir: &Path,
    workspace_id: &SpaceId,
    workspaces: &[Workspace],
) -> Result<()> {
    let marked = marked_owner(git_dir)?;
    let identity = directory_identity(git_dir)?;
    for other in workspaces {
        let owns = owns_git_dir(other, git_dir, Some(&identity), marked.as_deref())?;
        ensure!(
            other.id == *workspace_id || !owns,
            "Git worktree is already owned by {}; restore its recorded path",
            other.name
        );
    }
    if marked.is_some_and(|owner| owner != *workspace_id) {
        clear_owner(git_dir)?;
    }
    Ok(())
}

/// Mark the admin directory as `workspace_id`'s unless it already is,
/// returning whether it already was, and its filesystem identity.
fn mark_git_dir(git_dir: &Path, workspace_id: &SpaceId) -> Result<(bool, String)> {
    let marked = marked_owner(git_dir)?.as_deref() == Some(workspace_id);
    if !marked {
        mark_owner(git_dir, workspace_id.as_str())?;
    }
    // The identity only proves ownership of records made before markers.
    Ok((marked, directory_identity(git_dir)?))
}

/// Where the worktree of the workspace's recorded admin `directory` moved,
/// if the directory proves it is still the workspace's.
fn moved_worktree(workspace: &Workspace, directory: &Path) -> Result<Option<PathBuf>> {
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

#[cfg(test)]
mod tests;
