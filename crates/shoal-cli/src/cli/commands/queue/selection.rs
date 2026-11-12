//! The entry a `shoal queue` command acts on, named or picked.
use anyhow::{Context as _, Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{commands::queued_agents, context::Context, ui},
    model::QueueEntry,
    state::{QueueKind, QueueState},
};

/// The waiting entries in queue order.
pub(super) async fn waiting(ctx: &Context) -> Result<Vec<QueueEntry>> {
    let mut entries = queued_agents::entries(ctx).await?;
    entries.retain(|entry| entry.state == QueueState::Queued);
    Ok(entries)
}

/// The entry `selector` names, or one picked among the waiting entries.
pub(super) async fn selected(
    ctx: &Context,
    selector: Option<String>,
    prompt: &str,
) -> Result<String> {
    selected_among(ctx, selector, prompt, |_| true).await
}

/// The entry `selector` names, or one picked among the waiting entries
/// `offered` keeps.
pub(super) async fn selected_among(
    ctx: &Context,
    selector: Option<String>,
    prompt: &str,
    offered: impl Fn(&QueueEntry) -> bool,
) -> Result<String> {
    if let Some(selector) = selector {
        return named(ctx, selector).await;
    }
    let mut waiting = waiting(ctx).await?;
    waiting.retain(|entry| offered(entry));
    ensure!(!waiting.is_empty(), "no agents wait in the queue");
    ui::require_argument(ctx, "queue entry", "entry")?;
    Ok(pick_entry(ctx, &waiting, prompt).await?.id.into())
}

/// The entry `selector` names: an entry by its ID, else a space by its number
/// or ID, for its queued restore, as the notices of a stopped agent name it.
/// Anything else is left for the daemon to refuse.
async fn named(ctx: &Context, selector: String) -> Result<String> {
    let entries = queued_agents::entries(ctx).await?;
    if entries.iter().any(|entry| entry.id == selector) {
        return Ok(selector);
    }
    let Ok(space) = SpaceSelector::parse(&selector) else {
        return Ok(selector);
    };
    let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
    let Some(space) = spaces.find(&space) else {
        return Ok(selector);
    };
    let restore = entries.iter().find(|entry| {
        entry.kind == QueueKind::Restore && entry.workspace_id.as_ref() == Some(space.id())
    });
    Ok(restore.map_or(selector, |entry| entry.id.to_string()))
}

/// Pick one of `entries`.
pub(super) async fn pick_entry(
    ctx: &Context,
    entries: &[QueueEntry],
    prompt: &str,
) -> Result<QueueEntry> {
    let id = ui::pick(ctx, prompt, labelled(ctx, entries).await?)?;
    entries
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .context("picker returned an unknown entry")
}

/// `entries` as picker entries: each ID with its listing row.
async fn labelled(ctx: &Context, entries: &[QueueEntry]) -> Result<ui::Entries> {
    let rows = queued_agents::rows(ctx, entries).await?;
    Ok(entries
        .iter()
        .map(|entry| entry.id.to_string())
        .zip(rows)
        .collect())
}
