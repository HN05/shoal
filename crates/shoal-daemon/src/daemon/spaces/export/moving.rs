//! Moves that end spaces here only once their destination has them: the
//! daemon records the move, stops the agents of a space and of the spaces
//! it carries, saving their sessions, and writes their package while they
//! stay, then ends them through the export's removal once the destination
//! has the package, unless they changed meanwhile, or forgets a move that
//! failed. The record lets a daemon restarted during a move finish it.
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::{Prepared, carried, records};
use crate::{
    daemon::{executions::StopRequest, manager::Manager},
    model::Space,
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{
        Carrying, ExportedSpace, ManifestExt, MoveDestination, MovePhase, MoveRecord,
        StoppedExecutions,
    },
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Record the move of the space `selector` names, with the spaces
    /// below it that `carrying` carries, to `destination`, letting it
    /// delete the `unsaved` work the caller saw, then stop their agents,
    /// saving their sessions, and write them for another Shoal to the new
    /// file of the space's package there, leaving them here: each workspace
    /// at a new checkpoint, which the record keeps for
    /// [`Self::end_moved_space`]. A move already recorded, as one a
    /// restarted daemon finishes, packs again under its record.
    pub async fn export_moving_space(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        destination: &MoveDestination,
        carrying: &Carrying,
    ) -> Result<ExportedSpace> {
        let space = self.running_space(selector).await?;
        let file = self.move_package(&space.id, destination)?;
        crate::daemon::access::ensure_writable(crate::transfer::directory(&file)).await?;
        let mut record = self
            .record_space_move(&space, unsaved, destination, carrying)
            .await?;
        let prepared = self.prepare_moving(space, carrying).await?;
        let exported = self.write_prepared(prepared, file).await?;
        record.checkpoints = exported.manifest.checkpoints();
        record.phase = match destination {
            MoveDestination::Ssh(_) => MovePhase::Delivering,
            _ => MovePhase::Delivered,
        };
        let advanced = record.clone();
        (self.store)
            .write(move |tx| records::advance(tx, &advanced))
            .await?;
        Ok(exported)
    }

    /// End the space `selector` names, which its recorded move delivered,
    /// with the spaces below it that the move carries, through the
    /// export's removal, taking only the work the move's caller saw, while
    /// each workspace among them still has what its checkpoint in the
    /// package keeps, which the destination now has: before the removal and
    /// again once its removal hooks ran. Their issues' forge assignees stay
    /// for the spaces there, and the spaces below that a selection does not
    /// carry stay here. The move ends either way.
    pub async fn end_moved_space(&self, selector: &SpaceSelector) -> Result<RemovalResult> {
        let space = self.space(selector).await?;
        let mut record = self.recorded_move(&space.id).await?;
        record.phase = MovePhase::Delivered;
        let delivered = record.clone();
        (self.store)
            .write(move |tx| records::advance(tx, &delivered))
            .await?;
        let ended = self.end_delivered(&space, &record).await;
        self.forget_move(&record).await;
        ended
    }

    /// Forget the recorded move of the space `selector` names, which
    /// failed, returning the executions whose stopped work its spaces saved
    /// once it stopped their agents, which the caller restores here when the
    /// destination has nothing of them; none when nothing recorded it.
    pub async fn abandon_move(&self, selector: &SpaceSelector) -> Result<StoppedExecutions> {
        let id = match self.space(selector).await {
            Ok(space) => space.id,
            Err(error) => selector.given_id().cloned().ok_or(error)?,
        };
        let space = id.clone();
        let Some(record) = self.store.read(move |db| records::read(db, &space)).await? else {
            return Ok(StoppedExecutions::new());
        };
        let mut stopped = StoppedExecutions::new();
        for (space, before) in &record.saved_before {
            let Ok(saved) = self.saved_work(&SpaceSelector::id(space)).await else {
                continue;
            };
            let now = saved.execution_ids().into_iter();
            stopped.insert(
                space.clone(),
                now.filter(|id| !before.contains(id)).collect(),
            );
        }
        self.forget_move(&record).await;
        Ok(stopped)
    }

    /// The moves in progress.
    pub(crate) async fn recorded_moves(&self) -> Result<Vec<MoveRecord>> {
        self.store.read(records::all).await
    }

    /// Record the move of `space` and the spaces below it that `carrying`
    /// carries to `destination`, with the stopped work they saved so far,
    /// before any agent stops; a move already recorded for it in its first
    /// phase goes on under its record.
    async fn record_space_move(
        &self,
        space: &Space,
        unsaved: UnsavedWork,
        destination: &MoveDestination,
        carrying: &Carrying,
    ) -> Result<MoveRecord> {
        let id = space.id.clone();
        if let Some(recorded) = self.store.read(move |db| records::read(db, &id)).await? {
            anyhow::ensure!(
                recorded.phase == MovePhase::Packing,
                "the move of {} already wrote its package; it finishes from there",
                space.name
            );
            return Ok(recorded);
        }
        let carried = self.carried_by(&space.id, carrying).await?;
        let mut saved_before = StoppedExecutions::new();
        for moving in std::iter::once(space).chain(&carried) {
            let saved = self.saved_work(&SpaceSelector::id(&moving.id)).await?;
            let ids = saved.execution_ids().into_iter().collect();
            saved_before.insert(moving.id.clone(), ids);
        }
        let record = MoveRecord {
            space: space.id.clone(),
            destination: destination.clone(),
            carrying: carrying.clone(),
            unsaved,
            phase: MovePhase::Packing,
            checkpoints: Default::default(),
            saved_before,
        };
        let recorded = record.clone();
        (self.store)
            .write(move |tx| records::insert(tx, &recorded))
            .await?;
        Ok(record)
    }

    /// The recorded move of the space `id`.
    async fn recorded_move(&self, id: &SpaceId) -> Result<MoveRecord> {
        let space = id.clone();
        (self.store.read(move |db| records::read(db, &space)).await?)
            .with_context(|| format!("no move of space {id} is in progress"))
    }

    /// End `space` with the spaces its delivered move `record` carries.
    async fn end_delivered(&self, space: &Space, record: &MoveRecord) -> Result<RemovalResult> {
        let carried = self.carried_by(&space.id, &record.carrying).await?;
        let exported = &record.checkpoints;
        for below in &carried {
            self.ensure_moved_unchanged(below, exported).await?;
        }
        let unsaved = record.unsaved.clone();
        let moved = (self.moved_work(space, unsaved, exported.get(&space.id).copied())).await?;
        for handed in std::iter::once(space).chain(&carried) {
            self.hand_over_assignees(&handed.id).await?;
        }
        (self.leave_uncarried(space, &carried, &record.carrying)).await?;
        let inspection = InspectionPolicy::GitOnly;
        (self.remove_packaged(space, &carried, moved, inspection, Some(exported))).await
    }

    /// Forget the move `record` records, with the package it delivers to
    /// another machine from here.
    async fn forget_move(&self, record: &MoveRecord) {
        let space = record.space.clone();
        if let Err(error) = (self.store)
            .write(move |tx| records::forget(tx, &space))
            .await
        {
            crate::daemon::log!("forget the move of {}: {error:#}", record.space);
        }
        if let MoveDestination::Ssh(_) = record.destination {
            let name = crate::transfer::package_name(&record.space);
            let _ = std::fs::remove_file(self.paths.move_packages().join(name));
        }
    }

    /// The file the package of the space `id` goes to on its move to
    /// `destination`, creating the directory of packages delivered from
    /// here to another machine.
    fn move_package(&self, id: &SpaceId, destination: &MoveDestination) -> Result<PathBuf> {
        let staging = self.paths.move_packages();
        if let MoveDestination::Ssh(_) = destination {
            std::fs::create_dir_all(&staging)
                .with_context(|| format!("create {}", staging.display()))?;
        }
        Ok(destination.package(id, &staging))
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
