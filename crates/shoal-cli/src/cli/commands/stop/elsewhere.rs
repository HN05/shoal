//! Stopping the spaces other joined machines run, which the daemon's
//! selection names with their machines: each stop goes to that machine
//! through the daemon, and the hub holds it while that machine is offline.
use anyhow::Result;
use shoal_protocol::{
    hub::{directory::Listed, routing::Queued},
    model::Located,
    space_names::SpaceSelector,
};

use crate::{
    cli::{commands::machines, context::Context, output::Style},
    client::output::agents::SpaceStopped,
    model::Space,
    protocol::{QueuedLaunches, UntrackedProcesses},
};

/// The spaces of this machine among `located`.
pub(super) fn here(located: &[Located]) -> Vec<Space> {
    (located.iter())
        .filter_map(|located| match located {
            Located::Here(space) => Some((**space).clone()),
            Located::Elsewhere { .. } => None,
        })
        .collect()
}

/// What a stop of `located` checks before it stops any: each space here,
/// and those elsewhere on connected machines, since a stop for an offline
/// one waits for it unchecked.
pub(super) fn checked(located: &[Located]) -> Vec<(String, SpaceSelector)> {
    (located.iter())
        .filter_map(|located| match located {
            Located::Here(space) => Some((space.name.to_string(), SpaceSelector::id(&space.id))),
            Located::Elsewhere { space, online, .. } => (*online).then(|| {
                let space = &space.space;
                (space.name.to_string(), SpaceSelector::id(&space.id))
            }),
        })
        .collect()
}

/// Stop `space` on its machine, reporting it, or that the hub holds the
/// stop.
pub(super) async fn stop_one(ctx: &Context, space: &Listed, yes: bool) -> Result<()> {
    let selector = SpaceSelector::id(&space.space.id);
    // A process here is none of that machine's.
    let processes = UntrackedProcesses::Stop { requester: None };
    let stopped = super::stop(ctx, &selector, yes, QueuedLaunches::Cancel, processes).await;
    let stopped = match stopped {
        Ok(stopped) => stopped,
        Err(error) => match error.downcast::<Queued>() {
            Ok(queued) => return machines::queued(ctx, &queued).map(drop),
            Err(error) => return Err(error),
        },
    };
    let message = format!("Stopped {} on its machine", space.space.name);
    let report = SpaceStopped {
        stopped: true,
        saved: stopped.saved,
        other_processes: stopped.other_processes,
        cancelled: stopped.cancelled,
        cancelled_run: stopped.cancelled_run,
    };
    ctx.emit_styled(Style::Success, &message, report)
}
