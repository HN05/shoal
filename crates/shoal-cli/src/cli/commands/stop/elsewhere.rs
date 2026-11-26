//! Stopping the spaces other joined machines run. The hub holds a stop while
//! the machine is offline.
use anyhow::Result;
use shoal_protocol::{
    hub::{directory::Listed, routing::Queued},
    space_names::SpaceSelector,
};

use crate::{
    cli::{context::Context, output::Style},
    client::output::agents::SpaceStopped,
    protocol::{QueuedLaunches, UntrackedProcesses},
};

pub(super) async fn stop_one(ctx: &Context, space: &Listed, yes: bool) -> Result<Option<Queued>> {
    let selector = SpaceSelector::id(&space.space.id);
    // A process here is none of that machine's.
    let processes = UntrackedProcesses::Stop { requester: None };
    let stopped = super::stop(ctx, &selector, yes, QueuedLaunches::Cancel, processes).await;
    let stopped = match stopped {
        Ok(stopped) => stopped,
        Err(error) => return error.downcast::<Queued>().map(Some),
    };
    let message = format!("Stopped {} on its machine", space.space.name);
    let report = SpaceStopped {
        stopped: true,
        saved: stopped.saved,
        other_processes: stopped.other_processes,
        cancelled: stopped.cancelled,
        cancelled_run: stopped.cancelled_run,
    };
    ctx.emit_styled(Style::Success, &message, report)?;
    Ok(None)
}
