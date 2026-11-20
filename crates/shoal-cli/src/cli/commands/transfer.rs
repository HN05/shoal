//! `shoal move`: moving spaces to another Shoal, over SSH or through a file
//! that `shoal restore --from` continues. The daemon runs the move, so it
//! goes on when this command goes; the command picks what it moves and
//! where, asks before ending what needs the user, and follows it.
mod destination;
mod leaving;
mod stream;
mod targets;

use std::io::Write as _;

use anyhow::{Result, ensure};

use super::operations::Lines;
use crate::cli::{args::transfer::MoveArgs, context::Context, ui};
use crate::client::operations::Following;
use crate::protocol::operations::{Ended, Failure};
use crate::transfer::{Consent, MoveDestination, MoveOrder, MoveReport};
use destination::Chosen;

/// The file name that reads or writes the standard streams.
pub(super) const STANDARD_STREAM: &str = "-";

/// `shoal move`: move the spaces `args` selects to the destination it
/// names, or to the one the user picks, asking first where the move ends
/// more than the spaces named or releases holds.
pub(super) async fn run(ctx: &Context, args: MoveArgs) -> Result<i32> {
    let (spaces, chosen) = destination::choose(ctx, &args).await?;
    let mut order = MoveOrder {
        spaces,
        destination: chosen.destination.clone(),
        recursive: args.recursive(true),
        keep: args.keep,
        checkpoint: args.checkpoint,
        consent: args.confirmation.yes.then_some(Consent::Any),
    };
    if let MoveDestination::Client { .. } = order.destination {
        stream::ensure_survives(ctx, &order).await?;
    }
    let leaving = leaving::Leaving::find(ctx).await?;
    let report = loop {
        match follow(ctx, order.clone()).await? {
            Moved::Done(report) => break report,
            Moved::Asks(question) => {
                ensure!(ui::confirm(ctx, &question, "--yes")?, "move canceled");
                order.consent = Some(Consent::Answered(question));
            }
        }
    };
    leaving.navigate(ctx)?;
    finish(ctx, &chosen, report)
}

/// How a move the daemon ran ended: what it did, or the question it asks
/// before it ends anything.
enum Moved {
    Done(MoveReport),
    Asks(String),
}

/// Start `order` in the daemon of its spaces' machine and follow it to its
/// end, showing what it prints.
async fn follow(ctx: &Context, order: MoveOrder) -> Result<Moved> {
    let lines = match order.destination {
        MoveDestination::Client { .. } => Lines::Aside,
        _ => Lines::Command,
    };
    let env = crate::env::without_secrets();
    let (id, mut following) = Following::start_move(&ctx.paths, order, env).await?;
    let ended = super::operations::show(ctx, (&id, lines), async || following.next().await).await?;
    match ended {
        Ended::Succeeded(outcome) => Ok(Moved::Done(MoveReport::try_from(*outcome)?)),
        Ended::Failed(Failure::Unconfirmed { question }) => Ok(Moved::Asks(question)),
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
