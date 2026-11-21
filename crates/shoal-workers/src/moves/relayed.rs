//! A move to a file of a space another joined machine runs. That machine
//! packs the space and holds its package, which this one reads over the
//! hub and writes where the caller named; for a file it then ends the
//! space there, and the command that copies a package to its standard
//! output ends it once copied, as for a space of this machine.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail, ensure};

use super::Steps;
use crate::{
    client::{
        connection::CommandPaths,
        operations::{Followed, Following, MoveStarted},
    },
    protocol::operations::{Ended, Step},
};
use shoal_core::transfer::{MoveDestination, MoveOrder, MoveReport, SpaceMoved};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// Whether `order` moves to a file a space that this machine neither runs
/// nor removed, which another joined machine then runs.
pub(super) async fn elsewhere(paths: &CommandPaths, order: &MoveOrder) -> Result<bool> {
    let (MoveDestination::File { .. } | MoveDestination::Client { .. }) = order.destination else {
        return Ok(false);
    };
    let [id] = order.spaces.as_slice() else {
        return Ok(false);
    };
    Ok(!here(paths, id).await?)
}

/// Whether the space `id` runs here, or was removed here.
async fn here(paths: &CommandPaths, id: &SpaceId) -> Result<bool> {
    let selector = SpaceSelector::id(id);
    if crate::client::spaces::present_space(paths, &selector)
        .await?
        .is_some()
    {
        return Ok(true);
    }
    Ok(crate::client::removal::removed_space(paths, selector)
        .await
        .is_ok())
}

/// Run `order` for its space on the machine that runs it, holding its
/// package there, then write the package where `order` names, ending the
/// space there once a file holds it.
pub(super) async fn run(
    paths: &CommandPaths,
    order: &MoveOrder,
    steps: &impl Steps,
) -> Result<MoveReport> {
    ensure!(
        !order.keep,
        "a copy of another machine's space goes to a joined machine, not a file; copy it with --machine, or from that machine"
    );
    let held = MoveOrder {
        destination: MoveDestination::Held,
        ..order.clone()
    };
    let mut report = pack_there(paths, held).await?;
    let Some(packed) = report.moved.pop() else {
        return Ok(report);
    };
    steps.reached(Step::MoveDelivering).await;
    let file = package_file(&order.destination)?;
    let moved = deliver(paths, order, packed, file).await?;
    if let Some(done) = super::packages::done(order, &moved) {
        super::say(&done);
    }
    report.moved.push(moved);
    Ok(report)
}

/// Start the move `held` on the machine of its space and follow it to its
/// end there, passing on what it prints and the consent it asks for.
async fn pack_there(paths: &CommandPaths, held: MoveOrder) -> Result<MoveReport> {
    let mut following = match Following::start_move(paths, held, BTreeMap::new()).await? {
        MoveStarted::Following(_, following) => following,
        MoveStarted::AfterTurn(_) => bail!("a move to a file stops no agent's turn"),
    };
    loop {
        match following.next().await? {
            Followed::Event(_) => {}
            Followed::Ended(Ended::Succeeded(outcome)) => return MoveReport::try_from(*outcome),
            Followed::Ended(Ended::Failed(failure)) => {
                return Err(crate::operations::error(failure));
            }
        }
    }
}

/// The file `destination` names, which takes one space.
fn package_file(destination: &MoveDestination) -> Result<PathBuf> {
    match destination {
        MoveDestination::File { path } | MoveDestination::Client { path } => Ok(path.clone()),
        _ => bail!("only a move to a file reads its package from another machine"),
    }
}

/// Write the package of `packed`, which its machine holds, to `file`, and
/// end the space there when `order` moves it to a file this command keeps
/// no hold of.
async fn deliver(
    paths: &CommandPaths,
    order: &MoveOrder,
    mut packed: SpaceMoved,
    file: PathBuf,
) -> Result<SpaceMoved> {
    let selector = SpaceSelector::id(&packed.id);
    write_package(paths, selector.clone(), &file).await?;
    if let MoveDestination::File { .. } = order.destination {
        let ending = crate::client::transfer::end_moved(paths, selector).await;
        packed.removal = Some(ending.with_context(|| both_places(&file, &packed.id))?);
    }
    packed.file = Some(file);
    Ok(packed)
}

/// Write the package the machine of the space `space` holds to the new
/// file `file`, which goes again when the package does not arrive whole.
async fn write_package(paths: &CommandPaths, space: SpaceSelector, file: &Path) -> Result<()> {
    let mut writing =
        std::fs::File::create_new(file).with_context(|| format!("create {}", file.display()))?;
    let read = crate::client::transfer::read_held_package(paths, space, &mut writing).await;
    if let Err(error) = read {
        let _ = std::fs::remove_file(file);
        return Err(error.context("its package did not arrive, so it stays where it runs"));
    }
    Ok(())
}

/// What a move of the space `id`, whose package went to `file`, says when
/// the space could not end where it runs after all.
fn both_places(file: &Path, id: &SpaceId) -> String {
    format!(
        "it now exists where it runs and in {}; once the work you want is there, shoal rm {id} removes it there",
        file.display()
    )
}
