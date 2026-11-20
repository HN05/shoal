//! Moving spaces to another machine or a file, as the daemon's move
//! operation does: checking every space before any ends, and asking the
//! caller's consent where ending them needs it, then for each one packing
//! it, delivering its package and ending it here once its destination has
//! it, or with `--keep` delivering a copy of it. Only delivery differs by
//! destination.
pub mod checks;
mod confirmation;
mod copying;
mod finishing;
#[cfg(test)]
mod fixtures;
mod joined;
mod moving;
mod packages;
mod ssh;

use std::{io::Write as _, path::PathBuf};

use anyhow::{Context as _, Result, ensure};

use crate::{
    client::connection::CommandPaths,
    model::Space,
    protocol::operations::{Failure, Step},
};
use checks::Checked;
pub use finishing::finish;
use shoal_core::transfer::{MoveDestination, MoveFailure, MoveOrder, MoveReport, SpaceMoved};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::RemovedSpace;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// Where a move reports the step each space reached, and learns that it
/// was cancelled.
pub trait Steps: Sync {
    /// Report `step`, which a lost report never stops.
    fn reached(&self, step: Step) -> impl Future<Output = ()> + Send;

    /// Refuse to start on another space once the move is cancelled.
    fn ensure_running(&self) -> Result<()>;
}

/// A space a move takes, once checked, with the file its package goes to.
#[allow(
    clippy::large_enum_variant,
    reason = "one per moved space, kept until it moves"
)]
pub enum Planned {
    /// A live space the move ends here once its destination has it, with
    /// what that end does.
    Ending { checked: Checked, file: PathBuf },
    /// A live space a copy of which moves, leaving it running; its file is
    /// reserved as it goes, since a copy that fails keeps no other.
    Copied { space: Space },
    /// A removed space, whose removal record moves to a file.
    Removed { space: RemovedSpace, file: PathBuf },
}

impl Planned {
    /// The space's ID and name.
    pub fn identity(&self) -> (SpaceId, SpaceName) {
        match self {
            Self::Ending { checked, .. } => (checked.id().clone(), checked.name().clone()),
            Self::Copied { space } => (space.id.clone(), space.name.clone()),
            Self::Removed { space, .. } => (space.space_id.clone(), space.name.clone()),
        }
    }
}

/// Check every space `order` takes before any ends, refusing with each
/// one that cannot move, and plan the move of those that do not go with
/// another of them.
pub async fn plan(paths: &CommandPaths, order: &MoveOrder) -> Result<Vec<Planned>> {
    let tree = crate::client::spaces::list(paths).await?;
    let (present, removed) = ordered_spaces(paths, order, &tree).await?;
    let alone = checks::exported_alone(&present, &tree, &order.carrying());
    let mut planned = Vec::new();
    let mut refusals = Vec::new();
    for space in removed {
        let name = space.name.clone();
        match plan_removed(paths, order, space) {
            Ok(space) => planned.push(space),
            Err(error) => refusals.push(format!("{name}: {error:#}")),
        }
    }
    for space in alone {
        let name = space.name.clone();
        match plan_space(paths, order, space, &tree).await {
            Ok(space) => planned.push(space),
            Err(error) => refusals.push(format!("{name}: {error:#}")),
        }
    }
    ensure_takes(order, &planned, &refusals)?;
    Ok(planned)
}

/// The spaces `order` takes among those `tree` shows, and the removed ones
/// it names by ID.
async fn ordered_spaces(
    paths: &CommandPaths,
    order: &MoveOrder,
    tree: &crate::model::Spaces,
) -> Result<(Vec<Space>, Vec<RemovedSpace>)> {
    let (mut present, mut removed) = (Vec::new(), Vec::new());
    for id in &order.spaces {
        match tree.iter().find(|space| space.id == *id) {
            Some(space) => present.push(space.clone()),
            None => {
                let selector = SpaceSelector::id(id);
                let record = crate::client::removal::removed_space(paths, selector).await;
                removed.push(record.with_context(|| format!("no space {id}"))?);
            }
        }
    }
    Ok((present, removed))
}

/// Refuse a move whose spaces had `refusals`, or that cannot take the
/// `planned` spaces to its destination: a file takes one, and so does an
/// earlier checkpoint, which names one workspace's.
fn ensure_takes(order: &MoveOrder, planned: &[Planned], refusals: &[String]) -> Result<()> {
    let verb = match order.keep {
        true => "copy",
        false => "move",
    };
    ensure!(
        refusals.is_empty(),
        "cannot {verb} these spaces, so none was ended:\n  {}",
        refusals.join("\n  ")
    );
    ensure!(
        order.checkpoint.is_none() || planned.len() == 1,
        "--checkpoint copies one workspace, whose checkpoints it numbers; name one"
    );
    if let MoveDestination::File { .. } | MoveDestination::Client { .. } = order.destination {
        ensure!(
            planned.len() == 1,
            "several spaces go to a file each; name a directory with -o"
        );
    }
    Ok(())
}

/// Plan the move of the removed space `space` to a file, which a copy
/// refuses, since it keeps no running work.
fn plan_removed(paths: &CommandPaths, order: &MoveOrder, space: RemovedSpace) -> Result<Planned> {
    ensure!(!order.keep, "it is removed, so it keeps no work to copy");
    let file = packages::reserve(paths, &order.destination, &space.space_id)?;
    Ok(Planned::Removed { space, file })
}

/// Check `space` as `order` takes it, among the spaces `tree` shows, with
/// the package file of a space the move ends.
async fn plan_space(
    paths: &CommandPaths,
    order: &MoveOrder,
    space: Space,
    tree: &crate::model::Spaces,
) -> Result<Planned> {
    if order.keep {
        return Ok(Planned::Copied { space });
    }
    let file = packages::reserve(paths, &order.destination, &space.id)?;
    // A package for another machine waits in the staging directory.
    let into = match order.destination.machine() {
        Some(_) => None,
        None => Some(file.as_path()),
    };
    let checked = checks::check_export(paths, space, into, tree, &order.carrying()).await?;
    if let checks::Ending::Workspace(check) = &checked.space {
        check.ensure_settled()?;
    }
    Ok(Planned::Ending { checked, file })
}

/// Run `order`: check its spaces, then move each, or a copy of it, going on
/// after one fails, telling `steps` how far each got and printing a line
/// for each.
pub async fn run(paths: &CommandPaths, order: MoveOrder, steps: &impl Steps) -> Result<MoveReport> {
    let planned = plan(paths, &order).await?;
    ensure_consent(&order, &planned)?;
    if let MoveDestination::Ssh(destination) = &order.destination {
        ssh::Remote::new(destination).check().await?;
    }
    let several = planned.len() > 1;
    let mut report = MoveReport::default();
    for planned in planned {
        let (id, name) = planned.identity();
        let moved = match steps.ensure_running() {
            Ok(()) => move_planned(paths, &order, planned, steps).await,
            Err(cancelled) => Err(cancelled.context("it stays here")),
        };
        record(&order, &mut report, (id, name), moved);
    }
    if several || !report.failed.is_empty() {
        say(&packages::summary(&order, &report));
    }
    Ok(report)
}

/// Refuse, before anything ends, a move the caller did not consent to that
/// ends more than the spaces it names or releases holds, with the question
/// it asks.
fn ensure_consent(order: &MoveOrder, planned: &[Planned]) -> Result<()> {
    match confirmation::question(order, planned) {
        Some(question) if !order.consents_to(&question) => {
            Err(crate::operations::Failed(Failure::Unconfirmed { question }).into())
        }
        _ => Ok(()),
    }
}

/// Move the `planned` space as `order` says, or a copy of it.
async fn move_planned(
    paths: &CommandPaths,
    order: &MoveOrder,
    planned: Planned,
    steps: &impl Steps,
) -> Result<SpaceMoved> {
    match planned {
        Planned::Ending { checked, file } => {
            moving::move_one(paths, order, (checked, file), steps).await
        }
        Planned::Copied { space } => copying::copy_one(paths, order, &space, steps).await,
        Planned::Removed { space, file } => {
            packages::export_removed(paths, &space, file, &order.carrying()).await
        }
    }
}

/// Add how the move of the space `id`, `name`, went to `report`, printing
/// its line.
fn record(
    order: &MoveOrder,
    report: &mut MoveReport,
    (id, name): (SpaceId, SpaceName),
    moved: Result<SpaceMoved>,
) {
    match moved {
        Ok(moved) => {
            warn_left_out(&moved);
            if let Some(done) = packages::done(order, &moved) {
                say(&done);
            }
            report.moved.push(moved);
        }
        Err(error) => {
            let verb = if order.keep { "copy" } else { "move" };
            warn(&format!("error: cannot {verb} {name}: {error:#}"));
            let error = format!("{error:#}");
            report.failed.push(MoveFailure { id, name, error });
        }
    }
}

/// Print `line` as the operation's output, which a follower shows; a
/// follower that went, as with the daemon that relayed it, loses it.
fn say(line: &str) {
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// Print `line` as the operation's error output.
fn warn(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// Show what a package left out.
fn warn_left_out(moved: &SpaceMoved) {
    for warning in &moved.warnings {
        warn(&format!("warning: {warning}"));
    }
}
