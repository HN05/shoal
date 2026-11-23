//! A move to a file of a space another joined machine runs, or of a copy
//! of it. That machine packs the space and holds its package, which this
//! one reads over the hub and writes where the caller named; for a file it
//! then ends the space there, and the command that copies a package to its
//! standard output ends it once copied, as for a space of this machine. A
//! copy's package goes there once read, and the space keeps running.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};

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
/// package there, then write the package to the file `order` names, which
/// is refused before anything ends there when it cannot be created, ending
/// the space there once a file holds it.
pub(super) async fn run(
    paths: &CommandPaths,
    order: &MoveOrder,
    steps: &impl Steps,
) -> Result<MoveReport> {
    let reserved = Reserved::new(&order.destination)?;
    let held = MoveOrder {
        destination: MoveDestination::Held,
        ..order.clone()
    };
    let mut report = match pack_there(paths, held).await {
        Ok(report) => report,
        Err(error) => return Err(reserved.discard(error)),
    };
    let Some(packed) = report.moved.pop() else {
        reserved.remove();
        return Ok(report);
    };
    steps.reached(Step::MoveDelivering).await;
    let moved = deliver(paths, order, packed, reserved).await?;
    if let Some(done) = super::packages::done(order, &moved) {
        super::say(&done);
    }
    report.moved.push(moved);
    Ok(report)
}

/// Start the move `held` on the machine of its space and follow it to its
/// end there, passing on the consent it asks for.
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

/// The new file a move's package goes to, created before anything ends.
struct Reserved {
    path: PathBuf,
    file: std::fs::File,
}

impl Reserved {
    /// The new file `destination` names, which takes one space.
    fn new(destination: &MoveDestination) -> Result<Self> {
        let path = match destination {
            MoveDestination::File { path } | MoveDestination::Client { path } => path.clone(),
            _ => bail!("only a move to a file reads its package from another machine"),
        };
        let file = std::fs::File::create_new(&path)
            .with_context(|| format!("create {}; choose another file", path.display()))?;
        Ok(Self { path, file })
    }

    /// Delete the file, which takes no package.
    fn remove(self) {
        let _ = std::fs::remove_file(&self.path);
    }

    /// [`Self::remove`] the file, which takes no package for `error`.
    fn discard(self, error: anyhow::Error) -> anyhow::Error {
        self.remove();
        error
    }
}

/// Write the package of `packed`, which its machine holds, into `reserved`,
/// and end the space there when `order` moves it, rather than a copy, to a
/// file this command keeps no hold of.
async fn deliver(
    paths: &CommandPaths,
    order: &MoveOrder,
    mut packed: SpaceMoved,
    reserved: Reserved,
) -> Result<SpaceMoved> {
    let selector = SpaceSelector::id(&packed.id);
    let file = take_package(paths, selector.clone(), order.keep, reserved).await?;
    if let (MoveDestination::File { .. }, false) = (&order.destination, order.keep) {
        let ending = crate::client::transfer::end_moved(paths, selector).await;
        packed.removal = Some(ending.with_context(|| both_places(&file, &packed.id))?);
    }
    packed.file = Some(file);
    Ok(packed)
}

/// Write the package the machine of the space `space` holds into
/// `reserved`, a `copy`'s or a move's; one that does not arrive whole
/// leaves no file, and gives a move back there, where the space stays with
/// its agents' restores queued.
async fn take_package(
    paths: &CommandPaths,
    space: SpaceSelector,
    copy: bool,
    mut reserved: Reserved,
) -> Result<PathBuf> {
    let reading = crate::client::transfer::read_held_package;
    let Err(error) = reading(paths, space.clone(), copy, &mut reserved.file).await else {
        return Ok(reserved.path);
    };
    if !copy && let Err(released) = crate::client::transfer::release_held_move(paths, space).await {
        super::warn(&format!("warning: {released:#}"));
    }
    let error = error.context("its package did not arrive, so it stays where it runs");
    Err(reserved.discard(error))
}

/// What a move of the space `id`, whose package went to `file`, says when
/// the space could not end where it runs after all.
fn both_places(file: &Path, id: &SpaceId) -> String {
    format!(
        "it now exists where it runs and in {}; once the work you want is there, shoal rm {id} removes it there",
        file.display()
    )
}
