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

/// A space of another machine a stop takes.
pub(super) struct Remote {
    space: Listed,
    /// Its machine's link to the hub is open, so the stop can be checked.
    online: bool,
}

/// The spaces of this machine among `located`, and those of the others,
/// each in the order the selection gives.
pub(super) fn partition(located: Vec<Located>) -> (Vec<Space>, Vec<Remote>) {
    let mut here = Vec::new();
    let mut elsewhere = Vec::new();
    for located in located {
        match located {
            Located::Here(space) => here.push(*space),
            Located::Elsewhere { space, online, .. } => elsewhere.push(Remote {
                space: *space,
                online,
            }),
        }
    }
    (here, elsewhere)
}

/// What a stop of `here` and `elsewhere` checks before it stops any: each
/// space here, and those elsewhere on connected machines, since a stop for
/// an offline one waits for it unchecked.
pub(super) fn checked(here: &[Space], elsewhere: &[Remote]) -> Vec<(String, SpaceSelector)> {
    let here = (here.iter()).map(|space| (space.name.to_string(), SpaceSelector::id(&space.id)));
    let elsewhere = (elsewhere.iter().filter(|remote| remote.online)).map(|remote| {
        let space = &remote.space.space;
        (space.name.to_string(), SpaceSelector::id(&space.id))
    });
    here.chain(elsewhere).collect()
}

/// Stop `spaces`, which other machines run, each on its machine.
pub(super) async fn stop(ctx: &Context, spaces: Vec<Remote>, yes: bool) -> Result<()> {
    for remote in spaces {
        stop_one(ctx, &remote.space, yes).await?;
    }
    Ok(())
}

/// Stop `space` on its machine, reporting it, or that the hub holds the
/// stop.
async fn stop_one(ctx: &Context, space: &Listed, yes: bool) -> Result<()> {
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
