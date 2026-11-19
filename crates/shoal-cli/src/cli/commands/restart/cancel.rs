//! `shoal restart --cancel`: dropping the restarts the daemon keeps for
//! later, with the permission changes that never took effect.
use anyhow::Result;

use super::selection;
use crate::cli::{args::SpaceTargets, context::Context};
use crate::client::output::agents::CancelledRestarts;
use shoal_protocol::space_names::SpaceSelector;

/// Drop the recorded restart of each space `targets` selects, with `--all`
/// of each space that has one.
pub(in crate::cli::commands) async fn cancel(ctx: &Context, targets: SpaceTargets) -> Result<i32> {
    let all = targets.all;
    let spaces = selection::spaces(ctx, targets).await?;
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        selection::ensure_below(&spaces)?;
    }
    let mut cancelled = CancelledRestarts {
        cancelled: Vec::new(),
        unrecorded: Vec::new(),
    };
    for space in spaces
        .into_iter()
        .filter(|space| !all || space.restart.is_some())
    {
        let selector = SpaceSelector::id(&space.id);
        match crate::client::agents::cancel_restart(&ctx.paths, selector).await? {
            Some(_) => cancelled.cancelled.push(space.name),
            None => cancelled.unrecorded.push(space.name),
        }
    }
    ctx.emit(&summary(&cancelled), cancelled)?;
    Ok(0)
}

/// A line for each space, saying whether its restart was dropped.
fn summary(cancelled: &CancelledRestarts) -> String {
    let dropped =
        (cancelled.cancelled.iter()).map(|name| format!("Cancelled the restart of {name}"));
    let none = (cancelled.unrecorded.iter()).map(|name| format!("{name} has no restart to cancel"));
    let lines: Vec<String> = dropped.chain(none).collect();
    match lines.is_empty() {
        true => "No restarts to cancel".into(),
        false => lines.join("\n"),
    }
}
