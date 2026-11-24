//! `shoal restart --cancel`: dropping the restarts the daemon keeps for
//! later, with the permission changes that never took effect, here or on
//! another joined machine.
use anyhow::Result;

use super::{selection, target::on_machine};
use crate::cli::{args::SpaceTargets, context::Context, ui::Acting};
use crate::client::output::{
    agents::{CancelledRestarts, RestartSpace},
    batch::Failure,
};

/// Drop the recorded restart of each space `targets` selects; with `--all`
/// or `--machine`, reporting only the spaces that had one. A space whose
/// daemon cannot be asked, as on an offline machine, fails alone, exiting 1
/// once the others were asked.
pub(in crate::cli::commands) async fn cancel(ctx: &Context, targets: SpaceTargets) -> Result<i32> {
    let selector = targets.all || !targets.machines.is_empty();
    let spaces = selection::spaces(ctx, targets, &cancelling()).await?;
    let spaces = selection::into_targets(spaces);
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        selection::ensure_below(&spaces)?;
    }
    let mut cancelled = CancelledRestarts {
        cancelled: Vec::new(),
        unrecorded: Vec::new(),
        failed: Vec::new(),
    };
    for space in spaces
        .into_iter()
        .filter(|space| !selector || space.may_keep_restart)
    {
        let dropped = crate::client::agents::cancel_restart(space.selector());
        let dropped = dropped.send(&ctx.paths).await;
        let shown = space.shown();
        let reported = RestartSpace {
            machine: space.machine_name(),
            space: space.name,
        };
        match dropped {
            Ok(Some(_)) => cancelled.cancelled.push(reported),
            Ok(None) if !selector => cancelled.unrecorded.push(reported),
            Ok(None) => {}
            Err(error) => cancelled.failed.push(Failure {
                workspace: shown,
                error: format!("{error:#}"),
            }),
        }
    }
    let code = i32::from(!cancelled.failed.is_empty());
    ctx.emit(&summary(&cancelled), cancelled)?;
    Ok(code)
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
    let failed = (cancelled.failed.iter())
        .map(|failed| format!("Failed {}: {}", failed.workspace, failed.error));
    let lines: Vec<String> = dropped.chain(none).chain(failed).collect();
    match lines.is_empty() {
        true => "No restarts to cancel".into(),
        false => lines.join("\n"),
    }
}

/// What cancelling a space's recorded restart sends for each space a
/// picker picks.
pub(in crate::cli::commands) fn cancelling() -> Acting {
    Acting::request(crate::client::agents::cancel_restart)
}
