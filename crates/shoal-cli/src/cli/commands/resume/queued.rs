//! A queued restore the daemon started: `shoal resume` for the admitted
//! entry, where the agent stopped or in a background place, restores the
//! agent there, taking over the entry's slot.
use anyhow::{Result, ensure};
use shoal_workers::hosting;

use crate::{
    cli::{args::SpaceTargets, commands::queued_agents, context::Context},
    sessions::HostName,
    state::QueueState,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

/// Restore the stopped agent `execution` of the one space `targets` names
/// for the daemon's start of the queued restore `entry`, whose slot the
/// agent takes over.
pub async fn start_entry(
    ctx: &Context,
    targets: SpaceTargets,
    execution: Option<ExecutionId>,
    entry: shoal_ids::queue_entry_ids::QueueEntryId,
) -> Result<i32> {
    ensure!(
        !crate::process::scoped::inherits_scope(&ctx.paths.state),
        "resume agents outside scoped executions"
    );
    let ([space], Some(execution)) = (targets.space.as_slice(), execution) else {
        anyhow::bail!("a queued restore names one space and its stopped execution");
    };
    crate::env::launch_queue_entry(&entry);
    let _hold = queued_agents::Hold::start(&ctx.paths, &entry).await;
    // A stop or completion since admission cancelled it.
    let admitted = queued_agents::entries(ctx)
        .await?
        .iter()
        .any(|queued| queued.id == entry && queued.state == QueueState::Starting);
    ensure!(admitted, "the queued restore was cancelled");
    let restored = super::space::here(ctx, space.clone(), Some(execution), None).await;
    if let Err(error) = &restored {
        crate::queued_starts::fail(&ctx.paths, &entry, error).await;
    }
    restored
}

/// Record the place this restore runs in, one `host` opened in the
/// background for it, as the space's, before anything runs there.
pub async fn record_place(ctx: &Context, host: HostName, targets: &SpaceTargets) {
    if let Some(space) = background_space(targets) {
        hosting::record_owned_here(ctx.caller(), host, &space).await;
    }
}

/// Close the place this restore runs in, one `host` opened in the
/// background for it, once the one space `targets` names is done.
pub async fn close_place_when_done(ctx: &Context, host: HostName, targets: &SpaceTargets) {
    if let Some(space) = background_space(targets) {
        hosting::close_background_place_when_done(ctx.caller(), host, &space).await;
    }
}

/// The one space a restore in a background place names, which the
/// daemon's worker passes by its ID.
fn background_space(targets: &SpaceTargets) -> Option<SpaceId> {
    match targets.space.as_slice() {
        [space] => Some(SpaceId::from(space.to_string())),
        _ => None,
    }
}
