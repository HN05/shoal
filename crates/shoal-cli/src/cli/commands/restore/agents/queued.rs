//! The daemon's start of a queued restore: it starts `shoal resume` for the
//! admitted entry where the agent stopped or in a background place, and that
//! process restores the agent there, taking over the entry's slot.
use anyhow::{Result, ensure};
use std::ffi::OsString;

use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{args::SpaceTargets, commands::queued_agents, context::Context, terminal},
    model::QueueEntry,
    protocol::ConfigTarget,
    sessions::HostName,
    state::QueueState,
};

/// Restore the stopped agent `execution` of the one space `targets` names
/// for the daemon's start of the queued restore `entry`, whose slot the
/// agent takes over.
pub async fn start_entry(
    ctx: &Context,
    targets: SpaceTargets,
    execution: Option<ExecutionId>,
    entry: crate::client::queue::QueueEntryId,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
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
    let restored = super::space::run(ctx, space.clone(), Some(execution), true, None).await;
    if let Err(error) = &restored {
        queued_agents::fail(ctx, &entry, error).await;
    }
    restored
}

/// The daemon's worker for the admitted restore `entry`: restore its stopped
/// agent where it stopped, otherwise in the background, through
/// [`start_entry`].
pub(in crate::cli::commands) async fn start_queued(
    ctx: &Context,
    entry: &QueueEntry,
) -> Result<()> {
    let (Some(space), Some(execution)) = (&entry.space_id, &entry.execution) else {
        anyhow::bail!("the queued restore names no stopped agent");
    };
    let space = super::present_space(ctx, &SpaceSelector::id(space)).await?;
    let target = ConfigTarget::Space(SpaceSelector::id(space.id()));
    let settings = crate::config::settings(&ctx.paths, target).await?;
    let saved = crate::client::saved::read(&ctx.paths, SpaceSelector::id(space.id())).await?;
    let stopped = saved
        .session(execution)
        .and_then(|session| session.origin.as_ref());
    let args = [
        "resume",
        space.id().as_str(),
        "--execution",
        execution.as_str(),
        "--focus",
        "--queue-entry",
        entry.id.as_str(),
    ]
    .map(OsString::from);
    super::places::restore_queued(ctx, &settings, &space, stopped, &args).await
}

/// Record the place this restore runs in, one `host` opened in the
/// background for it, as the space's, before anything runs there.
pub async fn record_place(ctx: &Context, host: HostName, targets: &SpaceTargets) {
    if let Some(space) = background_space(targets) {
        terminal::record_owned_here(ctx, host, &space).await;
    }
}

/// Close the place this restore runs in, one `host` opened in the
/// background for it, once the one space `targets` names is done.
pub async fn close_place_when_done(ctx: &Context, host: HostName, targets: &SpaceTargets) {
    if let Some(space) = background_space(targets) {
        terminal::close_background_place_when_done(ctx, host, &space).await;
    }
}

/// The one space a restore in a background place names, which
/// [`start_queued`] passes by its ID.
fn background_space(targets: &SpaceTargets) -> Option<SpaceId> {
    match targets.space.as_slice() {
        [space] => Some(SpaceId::from(space.to_string())),
        _ => None,
    }
}
