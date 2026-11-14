//! Moves that end a space here only once another machine imported it: the
//! daemon stops the space's agents, saving their sessions, and writes its
//! package while the space stays, then ends it through the export's removal
//! once the caller reports that import, unless it changed meanwhile.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};

use super::{Prepared, ensure_exportable, packaged_record};
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::transfer::ManifestExt;
use crate::{
    daemon::workspace::{Manager, StopRequest, kept_session_sources},
    execution::recovery,
    model::{Chat, Checkpoint, CheckpointNumber, PresentSpace, Workspace},
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{ExportedSpace, Manifest, Source},
};

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
    /// space below it stays here, taking only the spaces below it that
    /// `unsaved` names, or a chat. Its issues' forge assignees stay for the
    /// space on the other machine.
    pub async fn end_moved_space(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        exported: Option<CheckpointNumber>,
    ) -> Result<Option<RemovalResult>> {
        let workspace = match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => workspace,
            PresentSpace::Chat(chat) => {
                self.hand_over_assignees(&chat.id).await?;
                (self.remove_chat(&SpaceSelector::id(&chat.id), Some(Vec::new()))).await?;
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

    /// Stop the agents of `workspace` and prepare its package at a new
    /// checkpoint, with every agent stopped there.
    async fn prepare_moving_workspace(&self, workspace: Workspace) -> Result<Prepared> {
        ensure_exportable(&workspace)?;
        // Refuses the chats in it or below it, which its end would take.
        self.exported_with(&workspace, &UnsavedWork::default())
            .await?;
        // The spaces below it moved first; one that stayed keeps it here,
        // before anything stops or reaches the other machine.
        self.ensure_nothing_below_stays(&workspace).await?;
        let selector = SpaceSelector::id(&workspace.id);
        self.stop_space(&selector, StopRequest::moving()).await?;
        let checkpoint = self.take_checkpoint(&selector, None).await?;
        ensure!(
            !checkpoint.files_omitted,
            "a checkpoint cannot keep the uncommitted files of {} (they are too large, or include a repository or a changed submodule); commit or remove them before moving it",
            workspace.name
        );
        let workspace = self.workspace_by_id(&workspace.id).await?;
        let agents = recovery::stopped_agents(&self.paths, &workspace.id)?;
        let mut record = packaged_record(&self.paths, &workspace, checkpoint, agents);
        record.session_files = self.unplaced_sessions(&workspace).await;
        let repo = self.repository(&workspace.repository_id).await?;
        Ok(Prepared {
            kept_sessions: kept_session_sources(&repo, &record),
            manifest: Manifest::of(&repo, Source::Workspace(record)).await?,
            // Its own config may describe its agents, and goes with it.
            settings: Some(self.workspace_settings(&workspace).await),
            repo,
            removal: None,
            continues: None,
        })
    }

    /// Stop the agent of `chat` and prepare its package, which continues
    /// the session the agent reported.
    async fn prepare_moving_chat(&self, chat: Chat) -> Result<Prepared> {
        let (repo, _) = self.movable_chat(&chat).await?;
        let selector = SpaceSelector::id(&chat.id);
        self.stop_space(&selector, StopRequest::moving()).await?;
        let chat = self.chat(&selector).await?;
        Ok(Prepared {
            manifest: Manifest::of(&repo, Source::Chat(chat)).await?,
            repo,
            removal: None,
            settings: None,
            kept_sessions: Vec::new(),
            continues: None,
        })
    }

    /// The checkpoint `exported` of `workspace`, refusing to end it once it
    /// differs from it, as after a commit made here since its export, or
    /// while a space below it stays here, which joined it since.
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
        self.ensure_nothing_below_stays(workspace).await?;
        Ok(checkpoint)
    }

    /// Refuse to move `workspace` while a space below it stays here, as one
    /// whose own move failed, which its end here would take.
    async fn ensure_nothing_below_stays(&self, workspace: &Workspace) -> Result<()> {
        let staying: Vec<String> = (self.spaces_taken_with(&workspace.id).await?)
            .into_iter()
            .map(|space| space.name.into())
            .collect();
        ensure!(
            staying.is_empty(),
            "spaces below workspace {} stay here ({})",
            workspace.name,
            staying.join(", ")
        );
        Ok(())
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
