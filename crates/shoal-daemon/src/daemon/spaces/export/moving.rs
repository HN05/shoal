//! Moves that end a space here only once another machine imported it: the
//! daemon stops the space's agents, saving their sessions, and writes its
//! package while the space stays, then ends it through the export's removal
//! once the caller reports that import, unless it changed meanwhile.
use std::path::PathBuf;

use anyhow::Result;

use super::{Prepared, worktree};
use crate::{
    daemon::{
        events::EventCause,
        workspace::{Manager, StopRequest},
    },
    model::Space,
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::ExportedSpace,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
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
        let space = self.running_space(selector).await?;
        let prepared = self.prepare_moving(space).await?;
        self.write_prepared(prepared, file).await
    }

    /// End the space `selector` names, which another machine imported,
    /// through the export's removal, taking only the spaces below it that
    /// `unsaved` names, once no space below it that does not go with it
    /// stays here and, for a workspace, it still has what its checkpoint
    /// `exported` keeps, which the other machine now has. Its issues' forge
    /// assignees stay for the space on the other machine.
    pub async fn end_moved_space(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        exported: Option<CheckpointNumber>,
    ) -> Result<RemovalResult> {
        let space = self.space(selector).await?;
        let moved = self.moved_work(&space, unsaved, exported).await?;
        // One that joined it since the export keeps it here.
        self.ensure_only_carried_below(&space.id, &space.name)
            .await?;
        let carried = self.chats_carried_by(&space.id).await?;
        self.hand_over_assignees(&space.id).await?;
        (self.remove_packaged(&space, &carried, moved, InspectionPolicy::GitOnly)).await
    }

    /// Stop the agents of `space` and of the chats that go with it and
    /// prepare its package, with every agent stopped there, as its removal
    /// would record it now: a workspace at a new checkpoint.
    async fn prepare_moving(&self, space: Space) -> Result<Prepared> {
        worktree::ensure_packable(&space)?;
        // The spaces below it that do not go with it moved first; one that
        // stayed keeps it here, before anything stops or reaches the other
        // machine.
        self.ensure_only_carried_below(&space.id, &space.name)
            .await?;
        // Its own config may describe its agents, and goes with it.
        let settings = self.space_settings(&space).await;
        let carried = self.chats_carried_by(&space.id).await?;
        (self.stop_space(&SpaceSelector::id(&space.id), StopRequest::moving())).await?;
        let chats = self.stopped_records(&carried).await?;
        let checkpoint = self.move_checkpoint(&space).await?;
        let record = self
            .present_removal_record(&space.id, checkpoint, EventCause::Manual)
            .await?;
        let repo = self.repository_of(record.repository_id.as_ref()).await?;
        let mut prepared = Prepared::of(self, repo, record, settings).await?;
        prepared.manifest.chats = chats;
        Ok(prepared)
    }
}
