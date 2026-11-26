//! `shoal queue start --attach`.
use anyhow::{Result, bail};

use crate::{
    cli::{
        commands::{queued_agents, sessions},
        context::Context,
    },
    model::QueueEntry,
};
use shoal_protocol::space_names::SpaceSelector;

/// Refuse to attach to the entry `selector` names when starting it starts
/// no agent's session, such as a CI job or a command, before anything
/// starts it.
pub(super) async fn require_session(ctx: &Context, selector: &str) -> Result<()> {
    let entries = queued_agents::entries(ctx).await?;
    let sessionless =
        (entries.iter()).find(|entry| entry.id == selector && !entry.kind.starts_session());
    match sessionless {
        Some(entry) => bail!("{} starts no agent session to attach", entry.name),
        None => Ok(()),
    }
}

pub(super) async fn started(ctx: &Context, entry: &QueueEntry) -> Result<i32> {
    let (workspace, session) =
        shoal_workers::attach::entry_session(&ctx.paths, entry, None).await?;
    sessions::attach(ctx, Some(SpaceSelector::id(&workspace)), Some(session)).await
}
