//! Moves that end spaces here only once another machine imported them: the
//! daemon stops the agents of a space and of the spaces it carries, saving
//! their sessions, and writes their package while they stay, then ends them
//! through the export's removal once the caller reports that import, unless
//! they changed meanwhile.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;

use super::{Prepared, carried};
use crate::{
    daemon::workspace::{Manager, StopRequest},
    model::Space,
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{Carrying, ExportedSpace},
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Stop the agents of the space `selector` names and of the spaces
    /// below it that `carrying` carries, saving their sessions, and write
    /// them for another Shoal to the new file `file`, leaving them here:
    /// each workspace at a new checkpoint, whose number the package's
    /// record holds for [`Self::end_moved_space`].
    pub async fn export_moving_space(
        &self,
        selector: &SpaceSelector,
        file: PathBuf,
        carrying: &Carrying,
    ) -> Result<ExportedSpace> {
        crate::daemon::access::ensure_writable(crate::transfer::directory(&file)).await?;
        let space = self.running_space(selector).await?;
        let prepared = self.prepare_moving(space, carrying).await?;
        self.write_prepared(prepared, file).await
    }

    /// End the space `selector` names, which another machine imported,
    /// with the spaces below it that `carrying` carries, through the
    /// export's removal, taking only those that `unsaved` names, once each
    /// workspace among them still has what its checkpoint in `exported`
    /// keeps, which the other machine now has. Their issues' forge
    /// assignees stay for the spaces on the other machine, and the spaces
    /// below that a selection does not carry stay here.
    pub async fn end_moved_space(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        exported: &BTreeMap<SpaceId, CheckpointNumber>,
        carrying: &Carrying,
    ) -> Result<RemovalResult> {
        let space = self.space(selector).await?;
        let carried = self.carried_by(&space.id, carrying).await?;
        for below in &carried {
            self.ensure_moved_unchanged(below, exported).await?;
        }
        let moved = (self.moved_work(&space, unsaved, exported.get(&space.id).copied())).await?;
        for handed in std::iter::once(&space).chain(&carried) {
            self.hand_over_assignees(&handed.id).await?;
        }
        self.leave_uncarried(&space, &carried, carrying).await?;
        (self.remove_packaged(&space, &carried, moved, InspectionPolicy::GitOnly)).await
    }

    /// Stop the agents of `space` and of the spaces below it that
    /// `carrying` carries and prepare their package, with every agent
    /// stopped there, as their removal would record them now: each
    /// workspace at a new checkpoint.
    async fn prepare_moving(&self, space: Space, carrying: &Carrying) -> Result<Prepared> {
        let carried = self.carried_by(&space.id, carrying).await?;
        carried::ensure_packable(&space, &carried)?;
        // Each one's own config may describe its agents, and goes with it.
        let settings = self
            .agent_settings_of(std::iter::once(&space).chain(&carried))
            .await;
        // Every agent stops before a checkpoint, since a chat may work in a
        // workspace's worktree.
        for moving in std::iter::once(&space).chain(&carried) {
            (self.stop_space(&SpaceSelector::id(&moving.id), StopRequest::moving())).await?;
        }
        let record = self.moving_record(&space).await?;
        let mut records = Vec::new();
        for below in &carried {
            records.push(self.moving_record(below).await?);
        }
        Prepared::of(self, record, records, settings).await
    }
}
