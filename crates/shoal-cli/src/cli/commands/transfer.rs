//! `shoal move`: moving spaces to another Shoal, over SSH or through a file
//! that `shoal restore --from` continues. The daemon runs the move, so it
//! goes on when this command goes; the command picks what it moves and
//! where, asks before ending what needs the user, and follows it.
mod confirmation;
mod destination;
mod leaving;
mod targets;

use std::io::Write as _;

use anyhow::Result;

use super::operations::Lines;
use crate::cli::{args::transfer::MoveArgs, context::Context};
use crate::client::operations::Following;
use crate::protocol::operations::Ended;
use crate::transfer::{MoveDestination, MoveOrder, MoveReport, OrderedSpace};
use destination::Chosen;
use shoal_workers::moves::{self, Planned};

/// The file name that reads or writes the standard streams.
pub(super) const STANDARD_STREAM: &str = "-";

/// `shoal move`: move the spaces `args` selects to the destination it
/// names, or to the one the user picks.
pub(super) async fn run(ctx: &Context, args: MoveArgs) -> Result<i32> {
    let (spaces, chosen) = destination::choose(ctx, &args).await?;
    let mut order = MoveOrder {
        spaces,
        destination: chosen.destination.clone(),
        recursive: args.recursive(true),
        keep: args.keep,
        checkpoint: args.checkpoint,
    };
    let planned = moves::plan(&ctx.paths, &order).await?;
    if let MoveDestination::Client { .. } = order.destination {
        confirmation::ensure_stream_survives(ctx, &planned).await?;
    }
    confirmation::confirm(ctx, args.confirmation.yes, &order, &planned)?;
    order.spaces = confirmed(&planned);
    let leaving = leaving::Leaving::find(ctx, &planned).await?;
    let report = follow(ctx, order).await?;
    leaving.navigate(ctx, &report)?;
    finish(ctx, &chosen, report)
}

/// The spaces of the `planned` move, each with the work the user saw that
/// ending it here may delete.
fn confirmed(planned: &[Planned]) -> Vec<OrderedSpace> {
    (planned.iter())
        .map(|planned| match planned {
            Planned::Ending { checked, .. } => OrderedSpace {
                space: checked.id().clone(),
                unsaved: checked.unsaved(),
            },
            Planned::Copied { space, .. } => OrderedSpace {
                space: space.id.clone(),
                unsaved: Default::default(),
            },
            Planned::Removed { space, .. } => OrderedSpace {
                space: space.space_id.clone(),
                unsaved: Default::default(),
            },
        })
        .collect()
}

/// Start `order` in the daemon of its spaces' machine and follow it to its
/// end, showing what it prints, returning what it did.
async fn follow(ctx: &Context, order: MoveOrder) -> Result<MoveReport> {
    let lines = match order.destination {
        MoveDestination::Client { .. } => Lines::Aside,
        _ => Lines::Command,
    };
    let env = crate::env::without_secrets();
    let (id, mut following) = Following::start_move(&ctx.paths, order, env).await?;
    let ended = super::operations::show(ctx, (&id, lines), async || following.next().await).await?;
    match ended {
        Ended::Succeeded(outcome) => MoveReport::try_from(*outcome),
        Ended::Failed(failure) => Err(crate::operations::error(failure)),
    }
}

/// Report the move that `report` describes, copying a package that goes to
/// standard output there, exiting 1 when any space failed to move.
fn finish(ctx: &Context, chosen: &Chosen, report: MoveReport) -> Result<i32> {
    if let MoveDestination::Client { path } = &chosen.destination {
        let [moved] = report.moved.as_slice() else {
            anyhow::bail!("the move wrote no package");
        };
        let mut file = std::fs::File::open(path)?;
        crate::stdout::write(|stdout| {
            std::io::copy(&mut file, stdout)?;
            stdout.flush()
        })?;
        match moved
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.copy.as_ref())
        {
            Some(_) => eprintln!("Exported a copy of {}", moved.name),
            None => eprintln!("Exported {}", moved.name),
        }
        return Ok(0);
    }
    let failed = !report.failed.is_empty();
    if ctx.json {
        outln!("{}", crate::client::output::json(&report)?);
    }
    Ok(if failed { 1 } else { 0 })
}
