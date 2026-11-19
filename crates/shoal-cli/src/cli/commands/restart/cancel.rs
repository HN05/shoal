//! `shoal restart --cancel`: dropping the restarts the daemon keeps for
//! later, with the permission changes that never took effect, here or on
//! another joined machine.
use anyhow::Result;

use super::{selection, target::on_machine};
use crate::cli::{args::SpaceTargets, context::Context};
use crate::client::output::agents::{CancelledRestarts, RestartSpace};

/// Drop the recorded restart of each space `targets` or `machines` selects;
/// with `--all` or `--machine`, reporting only the spaces that had one.
pub(in crate::cli::commands) async fn cancel(
    ctx: &Context,
    targets: SpaceTargets,
    machines: &[String],
) -> Result<i32> {
    let selector = targets.all || !machines.is_empty();
    let spaces = (selection::spaces(ctx, targets, machines).await?).into_targets();
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        selection::ensure_below(&spaces)?;
    }
    let mut cancelled = CancelledRestarts {
        cancelled: Vec::new(),
        unrecorded: Vec::new(),
    };
    for space in spaces
        .into_iter()
        .filter(|space| !selector || space.may_keep_restart)
    {
        let dropped = crate::client::agents::cancel_restart(&ctx.paths, space.selector()).await?;
        let reported = RestartSpace {
            machine: space.machine_name(),
            space: space.name,
        };
        match dropped {
            Some(_) => cancelled.cancelled.push(reported),
            None if !selector => cancelled.unrecorded.push(reported),
            None => {}
        }
    }
    ctx.emit(&summary(&cancelled), cancelled)?;
    Ok(0)
}

/// A line for each space, saying whether its restart was dropped.
fn summary(cancelled: &CancelledRestarts) -> String {
    let dropped = (cancelled.cancelled.iter()).map(|space| {
        format!(
            "Cancelled the restart of {}",
            on_machine(&space.space, space.machine.as_deref())
        )
    });
    let none = (cancelled.unrecorded.iter()).map(|space| {
        format!(
            "{} has no restart to cancel",
            on_machine(&space.space, space.machine.as_deref())
        )
    });
    let lines: Vec<String> = dropped.chain(none).collect();
    match lines.is_empty() {
        true => "No restarts to cancel".into(),
        false => lines.join("\n"),
    }
}
