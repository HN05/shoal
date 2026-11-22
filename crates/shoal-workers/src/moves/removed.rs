//! Moving a removed space's record to another machine joined to the same
//! hub: the daemon packs the record, as an export to a file does, and
//! delivers it, the destination records it as removed under its ID,
//! claiming it at the hub, and this machine forgets it once that claim
//! stands. When the destination cannot say whether it claimed it, the move
//! asks the hub, and the record stays here until the hub answers, its move
//! lock refusing its restore meanwhile.
use anyhow::{Result, anyhow, bail};

use super::{Steps, joined};
use crate::{client::connection::CommandPaths, protocol::operations::Step};
use shoal_core::transfer::{
    Carrying, MachineDestination, Manifest, MoveLock, MoveSettled, RecordArrival, RemovedDelivery,
    SpaceMoved,
};
use shoal_ids::{move_tickets::MoveTicket, space_ids::SpaceId};
use shoal_protocol::model::RemovedSpace;
use shoal_protocol::space_names::SpaceSelector;

/// Move the record of the removed space `space`, with the records of the
/// spaces its removal took that `carrying` carries, to the joined machine
/// `destination` names, telling `steps` how far it got, while it holds the
/// move lock of each record, which keeps its restore here off.
pub(super) async fn move_to_machine(
    paths: &CommandPaths,
    space: &RemovedSpace,
    destination: &MachineDestination,
    carrying: &Carrying,
    steps: &impl Steps,
) -> Result<SpaceMoved> {
    let mut held = HeldLocks::default();
    let moved = deliver(paths, space, (destination, carrying), (&mut held, steps)).await;
    held.release();
    moved
}

/// Have the daemon pack the record of `space` with those `carrying`
/// carries and deliver it to the joined machine `destination` names,
/// settling at the hub an arrival that machine could not report, taking
/// each record's move lock into `held` first.
async fn deliver(
    paths: &CommandPaths,
    space: &RemovedSpace,
    (destination, carrying): (&MachineDestination, &Carrying),
    (held, steps): (&mut HeldLocks, &impl Steps),
) -> Result<SpaceMoved> {
    let selector = SpaceSelector::id(&space.space_id);
    held.take_moving(paths, &space.space_id, carrying).await?;
    steps.reached(Step::MoveDelivering).await;
    let ticket = MoveTicket::generate();
    let delivering = crate::client::transfer::deliver_removed(
        paths,
        selector.clone(),
        destination.clone(),
        carrying.clone(),
        ticket.clone(),
    );
    let RemovedDelivery { exported, arrival } = delivering.await?;
    let machine = &destination.machine;
    match arrival {
        RecordArrival::Recorded => {}
        RecordArrival::Refused { error } => {
            bail!("machine {machine} took none of it, so its record stays here: {error}")
        }
        RecordArrival::Unknown { error } => {
            let carried = carried_ids(&exported.manifest);
            // Each record's lock keeps its restore here off until it settles.
            held.take_each(paths, &carried).await?;
            let records = (&selector, carried);
            settle(paths, records, (machine, &ticket), anyhow!(error), steps).await?
        }
    }
    Ok(SpaceMoved {
        id: space.space_id.clone(),
        name: space.name.clone(),
        manifest: Some(exported.manifest),
        file: None,
        removal: None,
        arrived: None,
        warnings: exported.warnings,
    })
}

/// The move locks a move of removed records holds, by the IDs of their
/// spaces, until it releases them all, whichever way it ends.
#[derive(Default)]
struct HeldLocks {
    ids: Vec<SpaceId>,
    locks: Vec<MoveLock>,
}

impl HeldLocks {
    /// Take the move locks of the removed space `id` and of the spaces its
    /// removal took that `carrying` carries, which its move takes along.
    async fn take_moving(
        &mut self,
        paths: &CommandPaths,
        id: &SpaceId,
        carrying: &Carrying,
    ) -> Result<()> {
        self.take_each(paths, std::slice::from_ref(id)).await?;
        let selector = SpaceSelector::id(id);
        let taken = crate::client::removal::taken_records(paths, selector, carrying.clone());
        let taken: Vec<SpaceId> = (taken.await?.into_iter())
            .map(|record| record.space_id)
            .collect();
        self.take_each(paths, &taken).await
    }

    /// Take the move lock of each space `ids` names that is not held yet.
    async fn take_each(&mut self, paths: &CommandPaths, ids: &[SpaceId]) -> Result<()> {
        for id in ids {
            if !self.ids.contains(id) {
                self.locks.push(MoveLock::take(paths, id).await?);
                self.ids.push(id.clone());
            }
        }
        Ok(())
    }

    /// Release every lock held.
    fn release(self) {
        self.locks.into_iter().for_each(MoveLock::release);
    }
}

/// The IDs of the spaces whose records the package `manifest` carries.
fn carried_ids(manifest: &Manifest) -> Vec<SpaceId> {
    (manifest.carried.iter())
        .map(|carried| carried.space.record.space_id.clone())
        .collect()
}

/// Settle at the hub the move `ticket` of the record of the removed space
/// `selector` names, with the records of the spaces `carried`, to the
/// joined machine named `machine`, whose arrival `error` left unknown:
/// once the hub says that machine claimed it, this machine forgot it, with
/// each carried record it claimed; otherwise it stays here.
async fn settle(
    paths: &CommandPaths,
    (selector, carried): (&SpaceSelector, Vec<SpaceId>),
    (machine, ticket): (&str, &MoveTicket),
    error: anyhow::Error,
    steps: &impl Steps,
) -> Result<()> {
    let ask = || {
        let (machine, ticket) = (machine.to_owned(), ticket.clone());
        let (space, carried) = (selector.clone(), carried.clone());
        crate::client::transfer::settle_removed_move(paths, space, carried, machine, ticket)
    };
    let meanwhile = "its record stays here";
    match joined::ask_hub(ask, &error, meanwhile, steps).await? {
        MoveSettled::Arrived => {
            super::warn(&format!(
                "warning: {error:#}; the hub says machine {machine} took it, so its record is there now"
            ));
            Ok(())
        }
        MoveSettled::Kept => Err(error.context("its record stays here")),
    }
}
