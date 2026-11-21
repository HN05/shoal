//! Verify the recorded Git worktree before execution, recovery, or removal.
use super::super::manager::Manager;
use crate::refusal::{Reason, Refusal};
use crate::{
    fsutil::off_runtime,
    git,
    model::{Checkout, Workspace},
};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use shoal_daemon_base::ownership::{
    clear_owner, mark_owner, marked_owner, paths::matches_recorded_worktree_path, verify_owner,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Whether a recorded workspace has a claim on the Git administrative
/// directory `git_dir`, whose owner marker is `marker`, that refuses
/// another's: its marker names it, or its record names the directory by any
/// path. Only the marker proves ownership; a recorded path alone keeps a
/// directory from passing to another workspace without a reclaim at it.
pub(super) fn claims_git_dir(workspace: &Workspace, git_dir: &Path, marker: Option<&str>) -> bool {
    marker == Some(workspace.id.as_str()) || records_git_dir(workspace, git_dir)
}

/// Whether the workspace's record names the admin directory `git_dir`,
/// comparing real paths where both exist.
fn records_git_dir(workspace: &Workspace, git_dir: &Path) -> bool {
    let real = fs::canonicalize(git_dir).ok();
    workspace.checkout.git_dir().is_some_and(|recorded| {
        match (real.as_ref(), fs::canonicalize(recorded).ok()) {
            (Some(real), Some(recorded)) => recorded == *real,
            _ => recorded == git_dir,
        }
    })
}

/// The Git directory every worktree of a repository shares, read from its
/// checkout when a verification first needs it and kept for the rest of
/// one operation, which verifies its worktree's own directories afresh each
/// time.
pub(crate) struct CommonDir {
    checkout: PathBuf,
    read: tokio::sync::OnceCell<PathBuf>,
}

impl CommonDir {
    pub(crate) fn of(checkout: &Path) -> Self {
        Self {
            checkout: checkout.to_owned(),
            read: tokio::sync::OnceCell::new(),
        }
    }

    /// The repository checkout it is read from.
    pub(crate) fn checkout(&self) -> &Path {
        &self.checkout
    }

    async fn path(&self) -> Result<&Path> {
        let read = self
            .read
            .get_or_try_init(|| git::common_dir(&self.checkout));
        Ok(read.await?)
    }
}

/// A worktree whose ownership verified.
pub(crate) struct VerifiedWorktree {
    /// Whether recording its admin directory changed it. A failure leaves the
    /// existing record and marker, which verification just accepted.
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
        let (_repo, _guard) = self.repositories.lock(workspace.repository()).await?;
        // The workspace may have gone while waiting for the repository.
        let workspace = self.workspace_by_id(&workspace.id).await?;
        self.reopen_owned_workspace(&workspace, workspace.branch().map(BranchName::as_str))
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
        crate::daemon::target_checks::ready(workspace, workspace.state).require()?;
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
        let actual = git::head_branch(&workspace.path).await?;
        ensure!(
            actual.as_ref().map(BranchName::as_str) == Some(expected_branch),
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
            shoal_links::pr::current_head(&workspace).await?;
        }
        Ok(workspace)
    }

    /// Verify that the worktree of `workspace` is the one it recorded,
    /// refusing a workspace whose worktree Git has yet to create.
    pub(crate) async fn verify_worktree(&self, workspace: &Workspace) -> Result<()> {
        self.verified_git_dir(workspace).await.map(drop)
    }

    /// The admin directory of the worktree of `workspace` once it verifies
    /// as [`Self::verify_worktree`] does, as the verification read it.
    pub(crate) async fn verified_git_dir(&self, workspace: &Workspace) -> Result<PathBuf> {
        let common_dir = (self.common_dir(workspace).await)
            .map_err(|error| Refusal::wrap(Reason::NotReady, error))?;
        self.verify_worktree_against(workspace, &common_dir).await
    }

    /// The admin directory of the worktree of `workspace` once it verifies
    /// as [`Self::verify_worktree`] does, as a worktree of the repository
    /// whose shared Git directory is `common_dir`.
    async fn verify_worktree_against(
        &self,
        workspace: &Workspace,
        common_dir: &CommonDir,
    ) -> Result<PathBuf> {
        if workspace.checkout.unborn() {
            let name = &workspace.name;
            let message =
                format!("workspace {name} has no worktree yet; its preparation creates it");
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        self.verify_worktree_identity(workspace, common_dir)
            .await
            .map_err(|error| Refusal::wrap(Reason::NotReady, error))
    }

    /// Verify the worktree a removal deletes as [`Self::verify_worktree`]
    /// does, except what is left of one Git disowned, once the admin
    /// directory of one a creation cut short left is recorded. What a
    /// workspace that recorded no admin directory holds at its path
    /// otherwise is the user's to delete. `common_dir` is the repository's
    /// shared Git directory.
    pub(crate) async fn verify_removed_worktree(
        &self,
        workspace: &Workspace,
        common_dir: &CommonDir,
    ) -> Result<()> {
        if self.disowned(workspace, common_dir.checkout()).await? {
            return Ok(());
        }
        let workspace = &self.recover_git_dir(workspace, common_dir).await?;
        let verified = (self.verify_worktree_against(workspace, common_dir).await).map(drop);
        if workspace.checkout.git_dir().is_some() {
            return verified;
        }
        verified.with_context(|| {
            format!(
                "nothing proves {} is the worktree of {}, which never recorded one; delete it if nothing there is needed, then run shoal rm again",
                workspace.path.display(),
                workspace.name
            )
        })
    }

    /// Verify the worktree a removal deletes as
    /// [`Self::verify_removed_worktree`] does, against `common_dir` where
    /// the removal read it, else reading it now.
    pub(crate) async fn verify_removed_worktree_with(
        &self,
        workspace: &Workspace,
        common_dir: Option<&CommonDir>,
    ) -> Result<()> {
        match common_dir {
            Some(common_dir) => self.verify_removed_worktree(workspace, common_dir).await,
            None => {
                let common_dir = self.common_dir(workspace).await?;
                self.verify_removed_worktree(workspace, &common_dir).await
            }
        }
    }

    /// Delete what a `git worktree add` for the pending `workspace`, without
    /// an admin directory recorded, left when it was cut short, as Git
    /// marks it until its checkout finishes, so that its creation adds the
    /// worktree again. `common_dir` is the repository's shared Git
    /// directory.
    pub(crate) async fn discard_unfinished_add(
        &self,
        workspace: &Workspace,
        common_dir: &CommonDir,
    ) -> Result<()> {
        if !workspace.checkout.unborn() {
            return Ok(());
        }
        let common_dir = common_dir.path().await?;
        if git::worktree::discard_unfinished_add(common_dir, &workspace.path).await? {
            crate::daemon::log!(
                "deleted what a cut-short worktree add left for {}",
                workspace.name
            );
        }
        Ok(())
    }

    /// Record the admin directory of the worktree Git created for the
    /// pending `workspace` that its creation, failed or cut short, did not
    /// record: the one among those of the repository, whose shared Git
    /// directory is `common_dir`, that records the workspace's path, once
    /// it is marked as the workspace's, which refuses one another workspace
    /// claims. A creation never marks it before recording it, so one marked
    /// for any other workspace is not its, nor one whose HEAD is not on the
    /// workspace's branch, or detached for a workspace at a commit, as a
    /// worktree someone else added at its path may be. Returns the workspace
    /// as it then stands; one that still has none has no worktree it can
    /// prove its own.
    pub(crate) async fn recover_git_dir(
        &self,
        workspace: &Workspace,
        common_dir: &CommonDir,
    ) -> Result<Workspace> {
        if !workspace.checkout.unborn() {
            return Ok(workspace.clone());
        }
        let common_dir = common_dir.path().await?;
        let Some(git_dir) = git::worktree::admin_dir_at(common_dir, &workspace.path).await? else {
            return Ok(workspace.clone());
        };
        if !self.created_for(workspace, &git_dir).await? {
            return Ok(workspace.clone());
        }
        self.release_stale_owner(&git_dir, &workspace.id).await?;
        self.record_git_dir(workspace, git_dir).await?;
        self.workspace_by_id(&workspace.id).await
    }

    /// Whether the admin directory `git_dir`, which records the path of the
    /// pending `workspace`, is the one its creation made: marked for no
    /// other workspace, with its HEAD where the creation put it.
    async fn created_for(&self, workspace: &Workspace, git_dir: &Path) -> Result<bool> {
        let marked = {
            let git_dir = git_dir.to_owned();
            off_runtime::run(move || marked_owner(&git_dir)).await?
        };
        if marked.is_some_and(|owner| owner != workspace.id.as_str()) {
            return Ok(false);
        }
        let branch = git::worktree::admin_dir_branch(git_dir).await?;
        Ok(branch.as_ref() == workspace.branch())
    }

    /// The admin directory of the worktree of `workspace` once it verifies
    /// as the one its record and marker name.
    async fn verify_worktree_identity(
        &self,
        workspace: &Workspace,
        common_dir: &CommonDir,
    ) -> Result<PathBuf> {
        let actual_git_dir = self.linked_git_dir(workspace, common_dir).await?;
        let workspace = workspace.clone();
        off_runtime::run(move || {
            verify_git_dir(&workspace, &actual_git_dir)?;
            Ok(actual_git_dir)
        })
        .await
    }

    /// Verify ownership, then record the worktree's admin directory when
    /// `record` is set. Recording runs only after verification passes; an error from
    /// verification means the worktree is not proven to be this workspace's.
    pub(crate) async fn verify_and_record_worktree(
        &self,
        workspace: &Workspace,
        record: bool,
    ) -> Result<VerifiedWorktree> {
        let git_dir = self.verified_git_dir(workspace).await?;
        let identity = if record {
            self.record_git_dir(workspace, git_dir).await
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
        let common_dir = self.common_dir(workspace).await?;
        let git_dir = self.linked_git_dir(workspace, &common_dir).await?;
        match workspace.branch() {
            Some(recorded) => {
                let branch = git::head_branch(&workspace.path)
                    .await
                    .context("worktree HEAD must be on the recorded branch")?;
                ensure!(
                    branch.as_ref().map(BranchName::as_str) == Some(recorded.as_str()),
                    "worktree is not on its recorded branch {recorded}; check it out before reclaiming"
                );
            }
            None => {
                let (_, branch) = git::head_and_branch(&workspace.path).await?;
                ensure!(
                    branch.is_none(),
                    "worktree is on branch {}, but its workspace is at a commit; detach it before reclaiming",
                    branch.as_ref().map_or("", BranchName::as_str)
                );
            }
        }
        self.release_stale_owner(&git_dir, &workspace.id).await?;
        self.record_git_dir(workspace, git_dir).await?;
        self.verify_worktree(&self.workspace_by_id(&workspace.id).await?)
            .await
    }

    /// Refuse an admin directory another recorded workspace claims, and
    /// clear a marker naming no recorded workspace: one
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

    /// The shared Git directory of the repository of `workspace`, read
    /// when first needed.
    pub(crate) async fn common_dir(&self, workspace: &Workspace) -> Result<CommonDir> {
        let repo = self.repositories.repository(workspace.repository()).await?;
        Ok(CommonDir::of(&repo.path))
    }

    /// The admin directory of the linked worktree rooted at the recorded path
    /// that belongs to the recorded repository, whose shared Git directory
    /// is `common_dir`.
    async fn linked_git_dir(
        &self,
        workspace: &Workspace,
        common_dir: &CommonDir,
    ) -> Result<PathBuf> {
        // Verify this is still the checkout Shoal created before any deletion.
        let (actual, expected) =
            tokio::join!(git::checkout_dirs(&workspace.path), common_dir.path());
        let actual = actual?;
        ensure!(
            actual.root == off_runtime::canonicalize(&workspace.path).await?,
            "workspace path no longer points to its worktree root"
        );
        let expected = expected?;
        ensure!(
            expected == actual.common_dir,
            "workspace now belongs to a different repository"
        );
        ensure!(
            actual.git_dir != actual.common_dir,
            "workspace was replaced by a main repository checkout"
        );
        Ok(actual.git_dir)
    }

    /// Mark the worktree's admin directory as this workspace's and record its
    /// location, returning whether anything changed.
    pub(crate) async fn record_worktree_identity(&self, workspace: &Workspace) -> Result<bool> {
        let git_dir = git::git_dir(&workspace.path).await?;
        self.record_git_dir(workspace, git_dir).await
    }

    /// Mark `git_dir`, the admin directory of the worktree of `workspace`
    /// as its caller read it, as this workspace's and record its location,
    /// returning whether anything changed.
    pub(crate) async fn record_git_dir(
        &self,
        workspace: &Workspace,
        git_dir: PathBuf,
    ) -> Result<bool> {
        let marked = {
            let (git_dir, id) = (git_dir.clone(), workspace.id.clone());
            off_runtime::run(move || mark_git_dir(&git_dir, &id)).await?
        };
        if workspace.checkout.git_dir() == Some(git_dir.as_path()) {
            return Ok(!marked);
        }
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET git_dir=?2 WHERE id=?1",
                    params![id, git_dir.to_str()],
                )?;
                Ok(true)
            })
            .await
    }

    /// Whether Git still lists the (possibly missing) worktree at its recorded path.
    pub(crate) async fn is_registered_worktree(&self, workspace: &Workspace) -> Result<bool> {
        let repo = self.repositories.repository(workspace.repository()).await?;
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
    /// A worktree whose creation is pending was never ready to be moved.
    pub(crate) async fn missing_worktree(&self, workspace: &Workspace) -> Result<Option<PathBuf>> {
        let Checkout::Created { git_dir, .. } = &workspace.checkout else {
            return Ok(None);
        };
        let directory = git_dir.clone();
        let workspace = workspace.clone();
        off_runtime::run(move || moved_worktree(&workspace, &directory)).await
    }
}

/// Refuse the admin directory `git_dir`, which `workspace` recorded, unless
/// its owner marker makes it the workspace's own, whatever its worktree
/// holds.
pub(in crate::daemon) async fn verify_admin_dir_owner(
    workspace: &Workspace,
    git_dir: &Path,
) -> Result<()> {
    let (git_dir, id) = (git_dir.to_owned(), workspace.id.clone());
    off_runtime::run(move || verify_owner(&git_dir, &id)).await
}

/// Refuse an admin directory that is not the workspace's own, or not the one
/// it recorded.
fn verify_git_dir(workspace: &Workspace, git_dir: &Path) -> Result<()> {
    verify_owner(git_dir, &workspace.id)?;
    if let Some(expected) = workspace.checkout.git_dir() {
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
    for other in workspaces {
        ensure!(
            other.id == *workspace_id || !claims_git_dir(other, git_dir, marked.as_deref()),
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
/// returning whether it already was.
fn mark_git_dir(git_dir: &Path, workspace_id: &SpaceId) -> Result<bool> {
    let marked = marked_owner(git_dir)?.as_deref() == Some(workspace_id.as_str());
    if !marked {
        mark_owner(git_dir, workspace_id.as_str())?;
    }
    Ok(marked)
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
        _ => verify_owner(directory, &workspace.id)?,
    }
    Ok(Some(path.to_owned()))
}

#[cfg(test)]
mod tests;
