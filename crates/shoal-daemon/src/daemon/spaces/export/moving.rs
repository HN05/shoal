//! Moves that end spaces here only once their destination has them: the
//! daemon records the move, stops the agents of a space and of the spaces
//! it carries, saving their sessions, and writes their package while they
//! stay, then ends them through the export's removal once the destination
//! has the package, unless they changed meanwhile, or forgets a move that
//! failed. The record lets a daemon restarted during a move finish it.
use std::{collections::HashSet, path::PathBuf, time::Duration};

use anyhow::{Context, Result};

use super::{Prepared, carried, records};
use crate::refusal::{Reason, Refusal};
use crate::{
    daemon::{executions::StopRequest, manager::Manager},
    model::Space,
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{
        Carrying, ExportedSpace, ManifestExt, MoveDestination, MovePhase, MoveRecord,
        StoppedExecutions,
    },
};
use shoal_ids::move_tickets::MoveTicket;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::time::UnixTime;

/// How long a move holds its package for the joined machine that started
/// it, after packing and after each read, before Shoal finishes it as one
/// whose reader went.
const HELD_FOR_READER: Duration = Duration::from_secs(30 * 60);

/// What a move packs a space for: its `destination`, the spaces below it
/// that go with it as `carrying` says, and the `unsaved` work the caller
/// saw that its end may delete; `again` packs again under the move's
/// record, as only the move's finisher does.
pub struct MovePacking {
    pub unsaved: UnsavedWork,
    pub destination: MoveDestination,
    pub carrying: Carrying,
    pub again: bool,
}

impl Manager {
    /// Record the move of the space `selector` names as `packing` says,
    /// then stop the agents of it and the spaces that go with it, saving
    /// their sessions, and write them for another Shoal to the new file of
    /// the space's package there, leaving them here: each workspace at a
    /// new checkpoint, which the record keeps for [`Self::end_moved_space`].
    /// Another move of the space that is recorded refuses it before
    /// anything else can, so its caller never takes that move for its own.
    pub async fn export_moving_space(
        &self,
        selector: &SpaceSelector,
        packing: MovePacking,
    ) -> Result<ExportedSpace> {
        let space = self.running_space(selector).await?;
        let resumed = self.resumed_move(&space, &packing).await?;
        let destination = &packing.destination;
        let file = self.move_package(&space.id, destination)?;
        shoal_daemon_base::writable::ensure_writable(crate::transfer::directory(&file)).await?;
        let mut record = match resumed {
            Some(record) => record,
            None => self.record_space_move(&space, &packing).await?,
        };
        let prepared = self.prepare_moving(space, &packing.carrying).await?;
        let exported = self.write_prepared(prepared, file).await?;
        record.checkpoints = exported.manifest.checkpoints();
        // A file holds the package once written; another machine, or the
        // command that copies it to its standard output, has it once it
        // took it, and the machine a held package waits for has nothing of
        // it until it read the whole package.
        record.phase = match destination {
            MoveDestination::File { .. } | MoveDestination::Directory { .. } => {
                MovePhase::Delivered
            }
            MoveDestination::Ssh(_)
            | MoveDestination::Machine(_)
            | MoveDestination::Client { .. } => MovePhase::Delivering,
            MoveDestination::Held => MovePhase::Packing,
        };
        self.advance_move(&record).await?;
        if *destination == MoveDestination::Held {
            self.hold_for_reader(&record.space).await?;
        }
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
        self.advance_move(&record).await?;
        let ended = self.end_delivered(&space, &record).await;
        self.forget_move(&record).await;
        ended
    }

    /// Forget the recorded move of the space `selector` names, which
    /// failed, returning the executions whose stopped work its spaces saved
    /// once it stopped their agents, which the caller restores here when the
    /// destination has nothing of them; none when nothing recorded it.
    pub async fn abandon_move(&self, selector: &SpaceSelector) -> Result<StoppedExecutions> {
        let id = self.moving_space_id(selector).await?;
        let Some(record) = self.move_record(&id).await? else {
            return Ok(StoppedExecutions::new());
        };
        let stopped = self.stopped_since(&record).await;
        self.forget_move(&record).await;
        Ok(stopped)
    }

    /// The ID of the space `selector` names, which a move may have ended:
    /// by its ID once it is gone.
    async fn moving_space_id(&self, selector: &SpaceSelector) -> Result<SpaceId> {
        match self.space(selector).await {
            Ok(space) => Ok(space.id),
            Err(error) => selector.given_id().cloned().ok_or(error),
        }
    }

    /// The executions whose stopped work each space `record` moves saved
    /// once its move stopped their agents, beyond what they saved before; a
    /// space that is gone saved none here.
    async fn stopped_since(&self, record: &MoveRecord) -> StoppedExecutions {
        let mut stopped = StoppedExecutions::new();
        for (space, before) in &record.saved_before {
            let Ok(saved) = self
                .agents
                .saved_work(self, &SpaceSelector::id(space))
                .await
            else {
                continue;
            };
            let now = saved.execution_ids().into_iter();
            let since = now.filter(|id| !before.contains(id)).collect();
            stopped.insert(space.clone(), since);
        }
        stopped
    }

    /// The moves in progress.
    pub(crate) async fn recorded_moves(&self) -> Result<Vec<MoveRecord>> {
        self.store.read(records::all).await
    }

    /// Record that the move of `record`'s space reached its phase.
    pub(crate) async fn advance_move(&self, record: &MoveRecord) -> Result<()> {
        let advanced = record.clone();
        (self.store)
            .write(move |tx| records::advance(tx, &advanced))
            .await
    }

    /// The spaces whose move holds its package for the joined machine that
    /// started it, which that machine may still read and end.
    pub(crate) async fn held_for_reader(&self) -> Result<HashSet<SpaceId>> {
        let now = UnixTime::now();
        self.store
            .read(move |db| records::held_after(db, now))
            .await
    }

    /// Have the move of the space `space`, which holds its package for the
    /// joined machine that started it, wait for that machine from now on.
    pub(crate) async fn hold_for_reader(&self, space: &SpaceId) -> Result<()> {
        let (space, until) = (space.clone(), UnixTime::now() + HELD_FOR_READER);
        (self.store)
            .write(move |tx| records::hold_until(tx, &space, until))
            .await
    }

    /// Record that the joined machine the move of the space `space` holds
    /// its package for read the whole package, which it may now have, and
    /// wait for it to end the space from now on.
    pub(crate) async fn held_package_read(&self, space: &SpaceId) -> Result<()> {
        let (space, until) = (space.clone(), UnixTime::now() + HELD_FOR_READER);
        (self.store)
            .write(move |tx| records::read_whole(tx, &space, until))
            .await
    }

    /// The recorded move of `space` that `packing`, its finisher's, packs
    /// again, one recorded in its first phase; none when no move of it is
    /// recorded, and any other move of a space with one is refused.
    async fn resumed_move(
        &self,
        space: &Space,
        packing: &MovePacking,
    ) -> Result<Option<MoveRecord>> {
        match self.move_record(&space.id).await? {
            None => Ok(None),
            Some(recorded) if packing.again && recorded.phase == MovePhase::Packing => {
                Ok(Some(recorded))
            }
            Some(_) => Err(move_in_progress(space).into()),
        }
    }

    /// Record the move of `space` and the spaces below it as `packing`
    /// says, with the stopped work they saved so far, before any agent
    /// stops.
    async fn record_space_move(&self, space: &Space, packing: &MovePacking) -> Result<MoveRecord> {
        let MovePacking {
            unsaved,
            destination,
            carrying,
            ..
        } = packing;
        let carried = self.carried_by(&space.id, carrying).await?;
        let mut saved_before = StoppedExecutions::new();
        for moving in std::iter::once(space).chain(&carried) {
            let saved = self
                .agents
                .saved_work(self, &SpaceSelector::id(&moving.id))
                .await?;
            let ids = saved.execution_ids().into_iter().collect();
            saved_before.insert(moving.id.clone(), ids);
        }
        let record = MoveRecord {
            space: space.id.clone(),
            destination: destination.clone(),
            ticket: MoveTicket::generate(),
            carrying: carrying.clone(),
            unsaved: unsaved.clone(),
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
    pub(crate) async fn recorded_move(&self, id: &SpaceId) -> Result<MoveRecord> {
        (self.move_record(id).await?)
            .with_context(|| format!("no move of space {id} is in progress"))
    }

    /// The recorded move of the space `id`, while one is recorded.
    pub(crate) async fn move_record(&self, id: &SpaceId) -> Result<Option<MoveRecord>> {
        let space = id.clone();
        self.store.read(move |db| records::read(db, &space)).await
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
            self.links.hand_over_assignees(&handed.id).await?;
        }
        let leaving = (&record.carrying, &record.destination);
        self.leave_uncarried(space, &carried, leaving).await?;
        let inspection = InspectionPolicy::GitOnly;
        let removal = self.remove_packaged(space, &carried, moved, inspection, Some(exported));
        let removed = removal.await;
        self.attach_behind(space, &carried).await;
        removed
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
        if record.destination.stages_package() {
            let name = crate::transfer::package_name(&record.space);
            let _ = std::fs::remove_file(self.paths.move_packages().join(name));
        }
    }

    /// The file the package of the space `id` goes to on its move to
    /// `destination`, creating the directory of packages delivered from
    /// here to another machine.
    fn move_package(&self, id: &SpaceId, destination: &MoveDestination) -> Result<PathBuf> {
        let staging = self.paths.move_packages();
        if destination.stages_package() {
            std::fs::create_dir_all(&staging)
                .with_context(|| format!("create {}", staging.display()))?;
        }
        Ok(destination.package(id, &staging))
    }

    /// Stop the agents of `space` and of the spaces below it that
    /// `carrying` carries and prepare their package, with every agent
    /// stopped there, as their removal would record them now: each
    /// workspace at a new checkpoint, with its pending review.
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
            (self
                .agents
                .stop_space(self, &SpaceSelector::id(&moving.id), StopRequest::moving()))
            .await?;
        }
        let record = self.moving_record(&space).await?;
        let mut records = Vec::new();
        for below in &carried {
            records.push(self.moving_record(below).await?);
        }
        Prepared::of(self, record, records, settings).await
    }
}

/// The refusal of a move of `space` while another move of it is recorded.
fn move_in_progress(space: &Space) -> Refusal {
    Refusal::new(
        Reason::MoveInProgress,
        format!(
            "a move of {} is recorded; Shoal finishes it, then it can move again",
            space.name
        ),
    )
}
