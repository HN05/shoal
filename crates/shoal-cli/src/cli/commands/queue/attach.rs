//! `shoal queue start --attach`.
use anyhow::{Result, bail};

use crate::{
    cli::{
        commands::{queued_agents, sessions},
        context::Context,
    },
    model::QueueEntry,
    state::QueueKind,
};
use shoal_protocol::space_names::SpaceSelector;

/// Refuse to attach to the entry `selector` names when it is a CI job,
/// which has no session, before anything starts it.
pub(super) async fn require_session(ctx: &Context, selector: &str) -> Result<()> {
    let entries = queued_agents::entries(ctx).await?;
    let job = (entries.iter()).find(|entry| entry.kind == QueueKind::Job && entry.id == selector);
    match job {
        Some(job) => bail!("{} is a CI job, which has no session to attach", job.name),
        None => Ok(()),
    }
}

pub(super) async fn started(ctx: &Context, entry: &QueueEntry) -> Result<i32> {
    let (workspace, session) =
        shoal_workers::attach::entry_session(&ctx.paths, entry, None).await?;
    sessions::attach(ctx, Some(SpaceSelector::id(&workspace)), Some(session)).await
}
