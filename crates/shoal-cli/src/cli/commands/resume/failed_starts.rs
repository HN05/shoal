//! `--focus` on a space whose agent's launch failed: the launch queues again as
//! it was and starts in this terminal.
use anyhow::Result;

use crate::{
    cli::{commands::queued_agents, context::Context},
    config::permissions::PermissionChange,
    model::QueueEntry,
    state::QueueState,
};
use shoal_protocol::protocol::RetryStart;
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn of(ctx: &Context, selector: &SpaceSelector) -> Result<Option<QueueEntry>> {
    let space = super::space::present_space(ctx, selector).await?;
    let entries = queued_agents::entries(ctx).await?.into_iter();
    Ok(entries
        .filter(|entry| entry.state == QueueState::Failed)
        .find(|entry| entry.space() == Some(&space.id)))
}

pub(super) async fn retry_here(
    ctx: &Context,
    failed: &QueueEntry,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let launches = std::slice::from_ref(failed);
    shoal_workers::resume::permissions::apply(&ctx.paths, &[], launches, change).await?;
    let retried =
        (crate::client::queue::retry(&ctx.paths, failed.id.clone(), RetryStart::Here)).await?;
    let _held = queued_agents::Hold::start(&ctx.paths, &retried.id).await;
    let entry = crate::client::queue::start_here(&ctx.paths, &retried.id).await?;
    queued_agents::run_here(ctx, &entry.id).await
}
