//! What exporting, copying and moving do with a workspace's worktree. A
//! chat works in a place it does not own and has none.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;

use crate::{
    model::{Checkpoint, RemovedSpace, Space, Workspace},
    removal::UnsavedWork,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    /// A workspace's checkpoint `number`, or a new one. Refuses one that
    /// could not keep the uncommitted files. None for a chat, which refuses
    /// a number.
    pub(super) async fn copy_checkpoint(
        &self,
        daemon: &dyn Daemon,
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
                self.take_checkpoint(daemon, &SpaceSelector::id(&workspace.id), None)
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

    /// A new checkpoint of a workspace, whose agents stopped. Refuses one that
    /// could not keep the uncommitted files. None for a chat.
    pub(super) async fn move_checkpoint(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
    ) -> Result<Option<Checkpoint>> {
        if space.worktree().is_none() {
            return Ok(None);
        }
        let checkpoint =
            (self.take_checkpoint(daemon, &SpaceSelector::id(&space.id), None)).await?;
        ensure!(
            !checkpoint.files_omitted,
            "a checkpoint cannot keep the uncommitted files of {} (they include a repository or a changed submodule); commit or remove them before moving it",
            space.name
        );
        Ok(Some(checkpoint))
    }

    /// For a workspace still unchanged since its checkpoint `exported`, which
    /// the other machine now has: what that checkpoint keeps, taking the
    /// spaces below that `unsaved` names. For a chat, which owns no files,
    /// `unsaved` itself.
    pub(super) async fn moved_work(
        &self,
        daemon: &dyn Daemon,
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
            .ensure_unchanged_since_export(daemon, &workspace, exported)
            .await?;
        Ok(checkpointed_work(&checkpoint, unsaved.children()))
    }

    /// Refuses a carried workspace that changed since its checkpoint in
    /// `exported`, which the other machine now has.
    pub(super) async fn ensure_moved_unchanged(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        exported: &BTreeMap<SpaceId, CheckpointNumber>,
    ) -> Result<()> {
        match space.to_workspace() {
            Some(workspace) => {
                self.ensure_moved_workspace_unchanged(daemon, &workspace, exported)
                    .await
            }
            None => Ok(()),
        }
    }

    /// The other machine now has the checkpoint in `exported`.
    pub async fn ensure_moved_workspace_unchanged(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        exported: &BTreeMap<SpaceId, CheckpointNumber>,
    ) -> Result<()> {
        let exported = (exported.get(&workspace.id).copied())
            .with_context(|| format!("the package of {} holds no checkpoint", workspace.name))?;
        self.ensure_unchanged_since_export(daemon, workspace, exported)
            .await?;
        Ok(())
    }

    /// Refuses once the workspace differs from it, as after a commit made
    /// here since the export.
    async fn ensure_unchanged_since_export(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        exported: CheckpointNumber,
    ) -> Result<Checkpoint> {
        let checkpoint = self.numbered_checkpoint(workspace, exported).await?;
        ensure!(
            self.unchanged_since(daemon, workspace, &checkpoint).await?,
            "workspace {} changed here after its export: its HEAD or unsaved files differ from checkpoint {exported}",
            workspace.name
        );
        Ok(checkpoint)
    }
}

/// Refuses a disposable workspace: the package is built from its removal
/// record, which a disposable workspace's removal does not keep.
pub(super) fn ensure_packable(space: &Space) -> Result<()> {
    let disposable = space.worktree().is_some_and(|worktree| worktree.disposable);
    ensure!(
        !disposable,
        "workspace {} is disposable, so its removal keeps nothing to export",
        space.name
    );
    Ok(())
}

pub(super) fn ensure_files_kept(removed: &RemovedSpace) -> Result<()> {
    let omitted = (removed.workspace()).is_some_and(|kept| kept.checkpoint.files_omitted);
    ensure!(
        !omitted,
        "the removal could not keep the uncommitted files; shoal restore {} opens the workspace here again",
        removed.space_id
    );
    Ok(())
}

/// The checkpoint's unsaved files include those the agents wrote as they
/// stopped after the caller's check, and the commits of its detached HEAD.
/// Takes only the spaces below that `children` names.
pub(super) fn checkpointed_work(
    checkpoint: &Checkpoint,
    children: Option<&[SpaceId]>,
) -> UnsavedWork {
    UnsavedWork::Confirmed {
        files: checkpoint.commit != checkpoint.head,
        detached_head: checkpoint.branch.is_none().then(|| checkpoint.head.clone()),
        files_restorable: false,
        children: children.map(<[SpaceId]>::to_vec),
    }
}
