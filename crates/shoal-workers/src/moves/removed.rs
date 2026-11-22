//! Moving a removed space's record to another machine joined to the same
//! hub: the daemon packs the record, as an export to a file does, and
//! delivers it, the destination records it as removed under its ID,
//! claiming it at the hub, and this machine forgets it once that claim
//! stands. When the destination cannot say whether it claimed it, the move
//! asks the hub, and the record stays here until the hub answers.
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
/// space's move lock.
pub(super) async fn move_to_machine(
    paths: &CommandPaths,
    space: &RemovedSpace,
    destination: &MachineDestination,
    carrying: &Carrying,
    steps: &impl Steps,
) -> Result<SpaceMoved> {
    let lock = MoveLock::take(paths, &space.space_id).await?;
    let moved = deliver(paths, space, (destination, carrying), steps).await;
    lock.release();
    moved
}

/// Have the daemon pack the record of `space` with those `carrying`
/// carries and deliver it to the joined machine `destination` names,
/// settling at the hub an arrival that machine could not report.
async fn deliver(
    paths: &CommandPaths,
    space: &RemovedSpace,
    (destination, carrying): (&MachineDestination, &Carrying),
    steps: &impl Steps,
) -> Result<SpaceMoved> {
    steps.reached(Step::MoveDelivering).await;
    let selector = SpaceSelector::id(&space.space_id);
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
            let records = (&selector, carried_ids(&exported.manifest));
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
