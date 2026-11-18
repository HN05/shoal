//! The worktree step of a package: what exporting, copying and moving a
//! workspace do with the worktree it owns, which a chat, working in a place
//! it does not own, has nothing of.
use anyhow::{Context, Result, ensure};

use crate::{
    daemon::workspace::Manager,
    model::{Checkpoint, RemovedSpace, Space, Workspace},
    removal::UnsavedWork,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// The checkpoint a copy of `space` holds: a workspace's checkpoint
    /// `number`, or a new one, refusing one that could not keep the
    /// uncommitted files; none for a chat, which refuses a number.
    pub(super) async fn copy_checkpoint(
        &self,
        space: &Space,
        number: Option<CheckpointNumber>,
    ) -> Result<Option<Checkpoint>> {
        let Some(workspace) = space.to_workspace() else {
            ensure!(
                number.is_none(),
                "chat {} has no checkpoints; export a copy of it without --checkpoint",
                space.name
            );
            return Ok(None);
        };
        let checkpoint = match number {
            Some(number) => self.numbered_checkpoint(&workspace, number).await?,
            None => {
                self.take_checkpoint(&SpaceSelector::id(&workspace.id), None)
                    .await?
            }
        };
        ensure!(
            !checkpoint.files_omitted,
            "checkpoint {} of {} could not keep the uncommitted files, so a copy of it would lose them",
            checkpoint.number,
            workspace.name
        );
        Ok(Some(checkpoint))
    }

    /// The checkpoint a move of `space`, whose agents stopped, holds: a new
    /// one of a workspace, refusing one that could not keep the uncommitted
    /// files; none for a chat.
    pub(super) async fn move_checkpoint(&self, space: &Space) -> Result<Option<Checkpoint>> {
        if space.worktree().is_none() {
            return Ok(None);
        }
        let checkpoint = (self.take_checkpoint(&SpaceSelector::id(&space.id), None)).await?;
        ensure!(
            !checkpoint.files_omitted,
            "a checkpoint cannot keep the uncommitted files of {} (they include a repository or a changed submodule); commit or remove them before moving it",
            space.name
        );
        Ok(Some(checkpoint))
    }

    /// Consent to delete what ending the moved `space` here deletes: for a
    /// workspace still unchanged since its checkpoint `exported`, which the
    /// other machine now has, what that checkpoint keeps, taking the spaces
    /// below that `unsaved` names; for a chat, which owns no files,
    /// `unsaved` itself.
    pub(super) async fn moved_work(
        &self,
        space: &Space,
        unsaved: UnsavedWork,
        exported: Option<CheckpointNumber>,
    ) -> Result<UnsavedWork> {
        let Some(workspace) = space.to_workspace() else {
            return Ok(unsaved);
        };
        let exported =
            exported.context("ending a moved workspace needs the checkpoint its package holds")?;
        let checkpoint = self
            .ensure_unchanged_since_export(&workspace, exported)
            .await?;
        Ok(checkpointed_work(&checkpoint, unsaved.children()))
    }

    /// The checkpoint `exported` of `workspace`, refusing to end it once it
    /// differs from it, as after a commit made here since its export.
    async fn ensure_unchanged_since_export(
        &self,
        workspace: &Workspace,
        exported: CheckpointNumber,
    ) -> Result<Checkpoint> {
        let checkpoint = self.numbered_checkpoint(workspace, exported).await?;
        ensure!(
            self.unchanged_since(workspace, &checkpoint).await?,
            "workspace {} changed here after its export: its HEAD or unsaved files differ from checkpoint {exported}",
            workspace.name
        );
        Ok(checkpoint)
    }
}

/// Refuse to package `space` when it is a disposable workspace: the package
/// is built from its removal record, which a disposable workspace's removal
/// does not keep.
pub(super) fn ensure_packable(space: &Space) -> Result<()> {
    let disposable = space.worktree().is_some_and(|worktree| worktree.disposable);
    ensure!(
        !disposable,
        "workspace {} is disposable, so its removal keeps nothing to export",
        space.name
    );
    Ok(())
}

/// Refuse to package the workspace `removed` records when its removal could
/// not keep its uncommitted files.
pub(super) fn ensure_files_kept(removed: &RemovedSpace) -> Result<()> {
    let omitted = (removed.workspace()).is_some_and(|kept| kept.checkpoint.files_omitted);
    ensure!(
        !omitted,
        "the removal could not keep the uncommitted files; shoal restore {} opens the workspace here again",
        removed.space_id
    );
    Ok(())
}

/// Consent to delete what `checkpoint` keeps, which the other machine
/// imported: its unsaved files, including those the agents wrote as they
/// stopped after the caller's check, and the commits of its detached HEAD,
/// taking only the spaces below that `children` names.
fn checkpointed_work(checkpoint: &Checkpoint, children: Option<&[SpaceId]>) -> UnsavedWork {
    UnsavedWork::Confirmed {
        files: checkpoint.commit != checkpoint.head,
        detached_head: checkpoint.branch.is_none().then(|| checkpoint.head.clone()),
        files_restorable: false,
        children: children.map(<[SpaceId]>::to_vec),
    }
}
