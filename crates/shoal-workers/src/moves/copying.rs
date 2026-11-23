//! Copying one space, with copies of the spaces below it, which its
//! destination opens as new spaces, leaving it running here.
use anyhow::{Result, bail};

use super::{Steps, ssh::Remote};
use crate::{client::connection::CommandPaths, model::Space, protocol::operations::Step};
use shoal_core::transfer::{
    Arrived, ImportedMove, MachineDestination, MoveDestination, MoveOrder, SpaceMoved,
};
use shoal_protocol::space_names::SpaceSelector;

/// Write a copy of `space` as `order` says to its new package file, at the
/// checkpoint the order names or a new one, then hand it to another machine
/// when the order goes there, telling `steps` how far it got.
pub(super) async fn copy_one(
    paths: &CommandPaths,
    order: &MoveOrder,
    space: &Space,
    steps: &impl Steps,
) -> Result<SpaceMoved> {
    let file = super::packages::reserve_copy(paths, &order.destination, &space.id)?;
    steps.reached(Step::MovePacking).await;
    let selector = SpaceSelector::id(&space.id);
    let (checkpoint, carrying) = (order.checkpoint, order.carrying());
    let copying =
        crate::client::transfer::copy(selector, checkpoint, file.clone(), carrying).send(paths);
    let mut exported = copying.await?;
    if let Some(number) = checkpoint {
        exported.warnings.push(format!(
            "the agents' conversations go past checkpoint {number} and describe work its files do not contain"
        ));
    }
    let mut moved = SpaceMoved {
        id: space.id.clone(),
        name: space.name.clone(),
        manifest: Some(exported.manifest),
        file: Some(file.clone()),
        removal: None,
        arrived: None,
        warnings: exported.warnings,
    };
    let imported = match &order.destination {
        MoveDestination::Ssh(destination) => {
            steps.reached(Step::MoveDelivering).await;
            Remote::new(destination)
                .import(&file)
                .await
                .map_err(Into::into)
        }
        MoveDestination::Machine(destination) => {
            steps.reached(Step::MoveDelivering).await;
            deliver_copy(paths, space, destination).await
        }
        MoveDestination::File { .. }
        | MoveDestination::Directory { .. }
        | MoveDestination::Client { .. }
        | MoveDestination::Held => return Ok(moved),
    };
    // The copy's package, delivered or not, is of no further use here.
    let _ = std::fs::remove_file(&file);
    moved.arrived = Some(imported?);
    moved.file = None;
    Ok(moved)
}

/// Deliver the package of the copy of `space` to the joined machine
/// `destination` names, which reports where it arrived.
async fn deliver_copy(
    paths: &CommandPaths,
    space: &Space,
    destination: &MachineDestination,
) -> Result<Arrived> {
    let machine = &destination.machine;
    let selector = SpaceSelector::id(&space.id);
    let delivering = crate::client::transfer::deliver_copy(paths, selector, destination.clone());
    match delivering.await? {
        ImportedMove::Arrived { arrived, .. } => Ok(*arrived),
        ImportedMove::Refused { error } => bail!("machine {machine} took none of it: {error}"),
        ImportedMove::Failed { error } => {
            bail!("the copy is on machine {machine}, but opening it there failed: {error}")
        }
        ImportedMove::Recorded { .. } => {
            bail!("the copy is on machine {machine}, recorded as removed without opening")
        }
    }
}
