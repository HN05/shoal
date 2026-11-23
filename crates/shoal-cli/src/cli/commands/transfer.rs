//! `shoal move`: moving spaces to another Shoal, over SSH, to a machine
//! joined to the same hub, or through a file that `shoal restore --from`
//! continues. The daemon runs the move, so it
//! goes on when this command goes; the command picks what it moves and
//! where, asks before ending what needs the user, and follows it.
mod destination;
mod leaving;
mod stream;
mod targets;

pub(super) use targets::moving;

use anyhow::{Result, ensure};

use crate::cli::{args::transfer::MoveArgs, context::Context, ui};
use crate::client::operations::{Following, MoveStarted};
use crate::protocol::operations::{Ended, Failure, OperationRef};
use crate::transfer::{Consent, DeferredMove, MoveDestination, MoveOrder, MoveReport};
use shoal_protocol::space_names::SpaceSelector;
use shoal_workers::following::Lines;

/// The file name that reads or writes the standard streams.
pub(super) const STANDARD_STREAM: &str = "-";

/// `shoal move`: move the spaces `args` selects to the destination it
/// names, or to the one the user picks, asking first where the move ends
/// more than the spaces named or releases holds.
pub(super) async fn run(ctx: &Context, args: MoveArgs) -> Result<i32> {
    let (spaces, chosen) = destination::choose(ctx, &args).await?;
    // A space that stays here keeps a parent that moved to a joined
    // machine, which keeps its ID there.
    let joined = matches!(chosen.destination, MoveDestination::Machine(_));
    let mut order = MoveOrder {
        spaces,
        destination: chosen.destination.clone(),
        recursive: args.recursive(!joined),
        keep: args.keep,
        checkpoint: args.checkpoint,
        consent: args.confirmation.yes.then_some(Consent::Any),
        check_only: false,
    };
    let streaming = stream::Streaming::start(ctx, &order).await?;
    let leaving = leaving::Leaving::find(ctx).await?;
    let report = loop {
        match follow(ctx, order.clone()).await? {
            Moved::Done(report) => break report,
            Moved::Asks(question) => {
                ensure!(ui::confirm(ctx, &question, "--yes")?, "move canceled");
                order.consent = Some(Consent::Answered(question));
            }
            Moved::AfterTurn(deferred) => return after_turn(ctx, &deferred),
        }
    };
    if let Some(streaming) = streaming {
        return streaming.deliver(ctx, &order, report).await;
    }
    leaving.navigate(ctx)?;
    finish(ctx, report)
}

/// How a move the daemon ran ended: what it did, or the question it asks
/// before it ends anything; or that it waits for the end of the turn of
/// the agent it stops.
enum Moved {
    Done(MoveReport),
    Asks(String),
    AfterTurn(DeferredMove),
}

/// Start `order` in the daemon of its spaces' machine and follow it to its
/// end, showing what it prints.
async fn follow(ctx: &Context, order: MoveOrder) -> Result<Moved> {
    let lines = match order.destination {
        MoveDestination::Client { .. } => Lines::Aside,
        _ => Lines::Command,
    };
    let env = crate::env::without_secrets();
    let space = runs_in(ctx, &order).await?;
    let (id, mut following) = match Following::start_move(&ctx.paths, order, env).await? {
        MoveStarted::Following(id, following) => (id, following),
        MoveStarted::AfterTurn(deferred) => return Ok(Moved::AfterTurn(deferred)),
    };
    let operation = OperationRef { id, space };
    let shown = (&operation, lines);
    let ended = super::operations::show(ctx, shown, async || following.next().await).await?;
    match ended {
        Ended::Succeeded(outcome) => Ok(Moved::Done(MoveReport::try_from(*outcome)?)),
        Ended::Failed(Failure::Unconfirmed { question }) => Ok(Moved::Asks(question)),
        Ended::Failed(failure) => Err(crate::operations::error(failure)),
    }
}

/// The space whose machine runs the move `order`, where cancelling it
/// goes: its first space's, or none for this machine, which reads another
/// machine's space into a file here.
async fn runs_in(ctx: &Context, order: &MoveOrder) -> Result<Option<SpaceSelector>> {
    if shoal_workers::moves::runs_elsewhere(&ctx.paths, order).await? {
        return Ok(None);
    }
    Ok((order.spaces.first()).map(SpaceSelector::id))
}

/// Report that the move starts once the turn of the agent it stops,
/// the caller's, ends.
fn after_turn(ctx: &Context, deferred: &DeferredMove) -> Result<i32> {
    let message = "The move starts once this turn ends, since it stops your agent, whose session then continues where the space goes";
    ctx.emit(message, deferred)?;
    Ok(0)
}

/// Report the move that `report` describes, exiting 1 when any space
/// failed to move.
fn finish(ctx: &Context, report: MoveReport) -> Result<i32> {
    if ctx.json {
        outln!("{}", crate::client::output::json(&report)?);
    }
    Ok(if report.failed.is_empty() { 0 } else { 1 })
}
