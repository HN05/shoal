//! A restored workspace's place: its worktree, opened again on its branch
//! at the removed HEAD with the unsaved files its removal checkpoint keeps.
use shoal_ids::space_ids::SpaceId;
use shoal_refs::{BranchName, CommitId};
use std::path::Path;

use anyhow::{Result, bail, ensure};

use super::super::manager::Manager;
use super::super::workspace::commits::NewRecord;
use crate::{
    git,
    model::{Checkpoint, Head, OpenedWorkspace, RemovedSpace, Repository},
};

impl Manager {
    /// Open the worktree of the workspace `removed` records again, below
    /// `parent`, on its branch at the removed HEAD, or at that commit for a
    /// workspace without a branch, with its unsaved files, adding to
    /// `warnings` when another branch has to take it.
    pub(super) async fn reopen_worktree(
        &self,
        removed: &RemovedSpace,
        parent: Option<SpaceId>,
        warnings: &mut Vec<String>,
    ) -> Result<OpenedWorkspace> {
        let new = NewRecord {
            id: Some(removed.space_id.clone()),
            parent,
        };
        match removed.kept_workspace()?.head.clone() {
            Head::Commit(at) => Ok(OpenedWorkspace {
                workspace: (self.restore_commit_workspace(removed, new, at)).await?,
                reused: false,
            }),
            Head::Branch(branch) => {
                (self.restore_branch_workspace(removed, branch, new, warnings)).await
            }
        }
    }

    /// Open the removed workspace `removed` again as `new` places it, on its
    /// `branch` at the removed HEAD, with the unsaved files its checkpoint
    /// keeps, adding to `warnings` when another branch has to take it.
    async fn restore_branch_workspace(
        &self,
        removed: &RemovedSpace,
        branch: BranchName,
        new: NewRecord,
        warnings: &mut Vec<String>,
    ) -> Result<OpenedWorkspace> {
        let branch = self.restore_branch(removed, branch, warnings).await?;
        let base = self.restore_base(removed).await?;
        let new = super::super::workspace::NewWorkspace {
            options: crate::protocol::CreationOptions {
                name: Some(removed.name.clone()),
                skip_setup: removed.kept_workspace()?.setup_skipped,
                ..Default::default()
            },
            id: new.id,
            parent: new.parent,
            ..Default::default()
        };
        let checkpoint = removed.kept_workspace()?.checkpoint.number;
        let opened = self
            .open_branch_restoring(
                removed.repository()?,
                branch.as_str(),
                base,
                new,
                Some(checkpoint),
            )
            .await?;
        // The branch check released the repository gate, so another request
        // may have opened the branch meanwhile; that workspace got no files.
        ensure!(
            !opened.reused,
            "workspace {} opened branch {branch} during the restore",
            opened.workspace.name
        );
        Ok(opened)
    }

    /// The branch to open: the workspace's `branch`, or, when it no longer
    /// contains the removed HEAD, as when the workspace was removed during a
    /// rebase, one named after the short commit, with a warning in
    /// `warnings`.
    async fn restore_branch(
        &self,
        removed: &RemovedSpace,
        branch: BranchName,
        warnings: &mut Vec<String>,
    ) -> Result<BranchName> {
        let (repo, _guard) = self.lock_repository(removed.repository()?).await?;
        let checkpoint = &removed.kept_workspace()?.checkpoint;
        if self.ready_branch(&repo, &branch, checkpoint).await? {
            return Ok(branch);
        }
        let short = BranchName::from(checkpoint.head.abbreviated(7));
        ensure!(
            self.ready_branch(&repo, &short, checkpoint).await?,
            "neither branch {branch} nor {short} contains the removed HEAD {}; point one of them back at it to restore the workspace",
            checkpoint.head
        );
        warnings.push(format!(
            "branch {branch} no longer contains the removed HEAD, so the workspace reopens on branch {short}"
        ));
        Ok(short)
    }

    /// Ready `branch` to open at `checkpoint`'s HEAD: at it, or at a later
    /// commit when nothing unsaved has to be applied, created there when it
    /// does not exist; false when it no longer contains HEAD.
    async fn ready_branch(
        &self,
        repo: &Repository,
        branch: &BranchName,
        checkpoint: &Checkpoint,
    ) -> Result<bool> {
        let head = checkpoint.head.as_str();
        if let Some(workspace) = self.list_workspaces().await?.into_iter().find(|workspace| {
            workspace.repository() == &repo.id && workspace.branch() == Some(branch)
        }) {
            bail!("workspace {} already uses branch {branch}", workspace.name);
        }
        let local = git::local_ref(branch);
        if !git::ref_exists(&repo.path, &local).await? {
            git::branch::create(&repo.path, branch, head).await?;
            return Ok(true);
        }
        let tip = git::verify_revision(
            &repo.path,
            &format!("{local}^{{commit}}"),
            git::run_isolated,
        )
        .await?;
        let tip = tip.trim();
        if !git::is_ancestor(&repo.path, head, tip, git::isolated_command).await? {
            return Ok(false);
        }
        ensure!(
            tip == checkpoint.head || checkpoint.commit == checkpoint.head,
            "branch {branch} moved past the removed HEAD; open it with shoal new and apply the unsaved files with git cherry-pick --no-commit {}",
            checkpoint.commit
        );
        Ok(true)
    }

    /// The recorded base, so diffs and the stack link come back: its ref
    /// while it exists, otherwise its commit; `None` when neither remains.
    async fn restore_base(&self, removed: &RemovedSpace) -> Result<Option<String>> {
        let repo = self.repository(removed.repository()?).await?;
        let kept = removed.kept_workspace()?;
        if let Some(reference) = &kept.base_ref
            && git::ref_exists(&repo.path, reference).await?
        {
            return Ok(Some(reference.clone()));
        }
        let Some(commit) = &kept.base_commit else {
            return Ok(None);
        };
        let exists = git::verify_revision(
            &repo.path,
            &format!("{commit}^{{commit}}"),
            git::run_isolated,
        )
        .await
        .is_ok();
        Ok((exists.then(|| commit.clone())).map(String::from))
    }
}

/// The unsaved files of a removal's checkpoint, written into the new worktree.
pub(in crate::daemon) struct RestoredFiles {
    pub head: CommitId,
    pub snapshot: CommitId,
}

impl RestoredFiles {
    /// The unsaved files `checkpoint` keeps at its HEAD; none when it kept
    /// none.
    pub(in crate::daemon) fn of(checkpoint: &Checkpoint) -> Option<Self> {
        (checkpoint.commit != checkpoint.head).then(|| Self {
            head: checkpoint.head.clone(),
            snapshot: checkpoint.commit.clone(),
        })
    }
}

/// Write a removal checkpoint's files into a new worktree still at the removed
/// HEAD, before the workspace can be used.
pub(in crate::daemon) async fn write_files(worktree: &Path, files: &RestoredFiles) -> Result<()> {
    let head = git::revision(worktree, "HEAD", git::run).await?;
    ensure!(
        head.trim() == files.head,
        "the branch moved during the restore; apply the unsaved files with git cherry-pick --no-commit {}",
        files.snapshot
    );
    git::checkpoint::apply(worktree, files.snapshot.as_str()).await
}
