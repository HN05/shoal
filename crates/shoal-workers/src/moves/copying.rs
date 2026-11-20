//! Copying one space, with copies of the spaces below it, which its
//! destination opens as new spaces, leaving it running here.
use anyhow::Result;

use super::{Steps, ssh::Remote};
use crate::{client::connection::CommandPaths, model::Space, protocol::operations::Step};
use shoal_core::transfer::{MoveDestination, MoveOrder, SpaceMoved};
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
    let file = super::packages::reserve(paths, &order.destination, &space.id)?;
    steps.reached(Step::MovePacking).await;
    let selector = SpaceSelector::id(&space.id);
    let (checkpoint, carrying) = (order.checkpoint, order.carrying());
    let copying =
        crate::client::transfer::copy(paths, selector, checkpoint, file.clone(), carrying);
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
    let destination = match &order.destination {
        MoveDestination::Ssh(destination) => destination,
        MoveDestination::Machine(destination) => {
            anyhow::bail!(
                "Shoal delivers no copy to machine {} yet",
                destination.machine
            )
        }
        MoveDestination::File { .. }
        | MoveDestination::Directory { .. }
        | MoveDestination::Client { .. } => return Ok(moved),
    };
    steps.reached(Step::MoveDelivering).await;
    let imported = Remote::new(destination).import(&file).await;
    // The copy's package, delivered or not, is of no further use here.
    let _ = std::fs::remove_file(&file);
    moved.arrived = Some(imported?);
    moved.file = None;
    Ok(moved)
}
