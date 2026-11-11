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

/// Restore the stopped agent `execution` of the one workspace `targets`
/// names for the daemon's start of the queued restore `entry`, whose slot the
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
    let ([workspace], Some(execution)) = (targets.space.as_slice(), execution) else {
        anyhow::bail!("a queued restore names one workspace and its stopped execution");
    };
    crate::env::launch_queue_entry(&entry);
    let _hold = queued_agents::Hold::start(&ctx.paths, &entry).await;
    // A stop or completion since admission cancelled it.
    let admitted = queued_agents::entries(ctx)
        .await?
        .iter()
        .any(|queued| queued.id == entry && queued.state == QueueState::Starting);
    ensure!(admitted, "the queued restore was cancelled");
    let restored = super::workspace::run(ctx, workspace.clone(), Some(execution), true, None).await;
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
    let (Some(workspace), Some(execution)) = (&entry.workspace_id, &entry.execution) else {
        anyhow::bail!("the queued restore names no stopped agent");
    };
    let workspace = crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(workspace))
        .await?
        .workspace;
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let record = crate::execution::recovery::saved_path(&ctx.paths, &workspace.id, execution);
    let args = [
        "resume",
        workspace.id.as_str(),
        "--execution",
        execution.as_str(),
        "--focus",
        "--queue-entry",
        entry.id.as_str(),
    ]
    .map(OsString::from);
    terminal::restore_queued(ctx, &settings, &workspace, &record, &args).await
}

/// Record the place this restore runs in, one `host` opened in the
/// background for it, as the workspace's, before anything runs there.
pub async fn record_place(ctx: &Context, host: HostName, targets: &SpaceTargets) {
    if let Some(workspace) = background_workspace(targets) {
        terminal::record_owned_here(ctx, host, &workspace).await;
    }
}

/// Close the place this restore runs in, one `host` opened in the
/// background for it, once the one workspace `targets` names is done.
pub async fn close_place_when_done(ctx: &Context, host: HostName, targets: &SpaceTargets) {
    if let Some(workspace) = background_workspace(targets) {
        terminal::close_background_place_when_done(ctx, host, &workspace).await;
    }
}

/// The one workspace a restore in a background place names, which
/// [`start_queued`] passes by its ID.
fn background_workspace(targets: &SpaceTargets) -> Option<SpaceId> {
    match targets.space.as_slice() {
        [workspace] => Some(SpaceId::from(workspace.to_string())),
        _ => None,
    }
}
