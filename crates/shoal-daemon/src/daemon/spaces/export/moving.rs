//! Moves that end a space here only once another machine imported it: the
//! daemon stops the space's agents, saving their sessions, and writes its
//! package while the space stays, then ends it through the export's removal
//! once the caller reports that import, unless it changed meanwhile.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};

use super::{Prepared, ensure_exportable, packaged_record};
use crate::transfer::ManifestExt;
use crate::{
    daemon::workspace::{Manager, StopRequest},
    execution::recovery,
    model::{Chat, Checkpoint, PresentSpace, Workspace},
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{ExportedSpace, Manifest},
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Stop the agents of the space `selector` names, saving their
    /// sessions, and write it for another Shoal to the new file `file`,
    /// leaving it here: a workspace at a new checkpoint, whose number the
    /// package's record holds for [`Self::end_moved_space`].
    pub async fn export_moving_space(
        &self,
        selector: &SpaceSelector,
        file: PathBuf,
    ) -> Result<ExportedSpace> {
        crate::daemon::access::ensure_writable(crate::transfer::directory(&file)).await?;
        let prepared = match self.running_space(selector).await? {
            PresentSpace::Workspace(workspace) => self.prepare_moving_workspace(workspace).await?,
            PresentSpace::Chat(chat) => self.prepare_moving_chat(chat).await?,
        };
        self.write_prepared(prepared, file).await
    }

    /// End the space `selector` names, which another machine imported,
    /// through the export's removal: a workspace once it still has what its
    /// checkpoint `exported` keeps, which the other machine now has, and no
    /// space below it that does not go with it stays here, or a chat, taking
    /// only the spaces below it that `unsaved` names. Its issues' forge
    /// assignees stay for the space on the other machine.
    pub async fn end_moved_space(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        exported: Option<CheckpointNumber>,
    ) -> Result<Option<RemovalResult>> {
        let workspace = match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => workspace,
            PresentSpace::Chat(chat) => {
                let taking = self.chat_taking(&chat.id, &unsaved).await?;
                self.hand_over_assignees(&chat.id).await?;
                (self.remove_chat(&SpaceSelector::id(&chat.id), Some(taking))).await?;
                return Ok(None);
            }
        };
        let exported =
            exported.context("ending a moved workspace needs the checkpoint its package holds")?;
        let checkpoint = self
            .ensure_unchanged_since_export(&workspace, exported)
            .await?;
        let moved = moved_work(&checkpoint, unsaved.children());
        self.hand_over_assignees(&workspace.id).await?;
        let removal = self
            .remove_exported(&workspace, moved, InspectionPolicy::GitOnly)
            .await?;
        Ok(Some(removal))
    }

    /// Stop the agents of `workspace` and of the chats that go with it and
    /// prepare its package at a new checkpoint, with every agent stopped
    /// there.
    async fn prepare_moving_workspace(&self, workspace: Workspace) -> Result<Prepared> {
        ensure_exportable(&workspace)?;
        // The spaces below it that do not go with it moved first; one that
        // stayed keeps it here, before anything stops or reaches the other
        // machine.
        self.ensure_only_carried_below(&workspace.id, &workspace.name)
            .await?;
        let carried = self.chats_carried_by(&workspace.id).await?;
        let selector = SpaceSelector::id(&workspace.id);
        self.stop_space(&selector, StopRequest::moving()).await?;
        let chats = self.stopped_records(&carried).await?;
        let checkpoint = self.take_checkpoint(&selector, None).await?;
        ensure!(
            !checkpoint.files_omitted,
            "a checkpoint cannot keep the uncommitted files of {} (they are too large, or include a repository or a changed submodule); commit or remove them before moving it",
            workspace.name
        );
        let workspace = self.workspace_by_id(&workspace.id).await?;
        let agents = recovery::stopped_agents(&self.paths, &workspace.id)?;
        let mut record = packaged_record(&self.paths, &workspace, checkpoint, agents);
        let repo = self.repository(&workspace.repository_id).await?;
        let present = PresentSpace::Workspace(workspace.clone());
        record.session_files = self.unplaced_sessions(&present).await;
        let kept_sessions = self.kept_session_sources(&record).await;
        let mut manifest = Manifest::of(Some(&repo), record).await?;
        manifest.chats = chats;
        Ok(Prepared {
            kept_sessions,
            manifest,
            // Its own config may describe its agents, and goes with it.
            settings: Some(self.workspace_settings(&workspace).await),
            repo: Some(repo),
            removal: None,
        })
    }

    /// Stop the agents of `chat` and of the chats that go with it and
    /// prepare its package, which continues the session the agent reported,
    /// from the record its removal would keep.
    async fn prepare_moving_chat(&self, chat: Chat) -> Result<Prepared> {
        self.ensure_movable_chat(&chat).await?;
        let repo = self.chat_repository(&chat).await?;
        let carried = self.chats_carried_by(&chat.id).await?;
        let record = self.stopped_record(&chat.id).await?;
        let chats = self.stopped_records(&carried).await?;
        let kept_sessions = self.kept_session_sources(&record).await;
        let mut manifest = Manifest::of(repo.as_ref(), record).await?;
        manifest.chats = chats;
        Ok(Prepared {
            manifest,
            repo,
            removal: None,
            settings: None,
            kept_sessions,
        })
    }

    /// The checkpoint `exported` of `workspace`, refusing to end it once it
    /// differs from it, as after a commit made here since its export, or
    /// while a space below it that does not go with it stays here, which
    /// joined it since.
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
        (self.ensure_only_carried_below(&workspace.id, &workspace.name)).await?;
        Ok(checkpoint)
    }
}

/// Consent to delete what `checkpoint` keeps, which the other machine
/// imported: its unsaved files, including those the agents wrote as they
/// stopped after the caller's check, and the commits of its detached HEAD,
/// taking only the spaces below that `children` names.
fn moved_work(checkpoint: &Checkpoint, children: Option<&[SpaceId]>) -> UnsavedWork {
    UnsavedWork::Confirmed {
        files: checkpoint.commit != checkpoint.head,
        detached_head: checkpoint.branch.is_none().then(|| checkpoint.head.clone()),
        files_restorable: false,
        children: children.map(<[SpaceId]>::to_vec),
    }
}
