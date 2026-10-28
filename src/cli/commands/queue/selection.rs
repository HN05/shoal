//! The entry a `shoal queue` command acts on and the place a move takes,
//! named or picked.
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{commands::agent_queue, context::Context, ui},
    model::{QueueEntry, QueuePlace},
    state::QueueState,
};

/// The waiting entries in queue order.
pub(super) async fn waiting(ctx: &Context) -> Result<Vec<QueueEntry>> {
    let mut entries = agent_queue::entries(ctx).await?;
    entries.retain(|entry| entry.state == QueueState::Queued);
    Ok(entries)
}

/// The entry `selector` names, or one picked among the waiting entries.
pub(super) async fn selected(
    ctx: &Context,
    selector: Option<String>,
    prompt: &str,
) -> Result<String> {
    if let Some(selector) = selector {
        return Ok(selector);
    }
    let waiting = waiting(ctx).await?;
    ensure!(!waiting.is_empty(), "no agents wait in the queue");
    Ok(pick_entry(ctx, &waiting, prompt).await?.id)
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

/// Pick the place of the entry `selector` names, in front of another
/// waiting entry or last.
pub(super) async fn pick_place(ctx: &Context, selector: &str) -> Result<QueuePlace> {
    ensure!(
        ctx.interactive(),
        "name the position to move {selector} to, or last"
    );
    let (moved, others): (Vec<_>, Vec<_>) = waiting(ctx)
        .await?
        .into_iter()
        .partition(|entry| entry.id == selector || entry.name == selector);
    let name = moved.first().map_or(selector, |entry| entry.name.as_str());
    let place = ui::pick_place(
        ctx,
        &format!("Move {name} in front of> "),
        labelled(ctx, &others).await?,
    )?;
    Ok(match place {
        ui::Place::Before(id) => QueuePlace::Before(id),
        ui::Place::Last => QueuePlace::Last,
    })
}

/// `entries` as picker entries: each ID with its listing row.
async fn labelled(ctx: &Context, entries: &[QueueEntry]) -> Result<ui::Entries> {
    let rows = agent_queue::rows(ctx, entries).await?;
    Ok(entries
        .iter()
        .map(|entry| entry.id.clone())
        .zip(rows)
        .collect())
}
