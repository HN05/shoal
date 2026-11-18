//! `shoal start` launches under `max_heavy_agents`: admission, the entries the
//! queue lists, and the hold that keeps a starting launch's slot.

use anyhow::Result;

use super::listing::ListFilter;
use crate::client::queue;
use crate::{
    cli::{context::Context, output::Palette, ui},
    model::QueueEntry,
    state::{QueueKind, QueueState},
};
use shoal_ids::queue_entry_ids::QueueEntryId;

/// When the queued `entry` starts: once nothing blocks it and a heavy slot
/// is free.
pub(super) fn starts_when(entry: &QueueEntry) -> String {
    match entry.blockers.as_slice() {
        [] => "when a heavy slot is free".to_owned(),
        blockers => format!(
            "once it is no longer blocked by {} and a heavy slot is free",
            blockers.join(", ")
        ),
    }
}

pub(super) use crate::client::queue::Hold;

/// Run the launch `entry`, which this process marked starting for itself,
/// here, holding its slot until its agent takes it over.
pub(super) async fn run_here(ctx: &Context, entry: &QueueEntryId) -> Result<i32> {
    let (_, plan) = crate::queued_starts::launch_plan(&ctx.paths, entry).await?;
    super::workspaces::execute_add(ctx, plan).await
}

pub(super) async fn entries(ctx: &Context) -> Result<Vec<QueueEntry>> {
    queue::entries(&ctx.paths).await
}

/// `launch` as the queue has it now, gone once its agent started.
pub(super) async fn current(ctx: &Context, launch: &QueueEntry) -> Result<Option<QueueEntry>> {
    let entries = entries(ctx).await?;
    Ok(entries.into_iter().find(|entry| entry.id == launch.id))
}

/// The entries `shoal list` and `shoal queue` show under `filter`.
pub(super) async fn listed(ctx: &Context, filter: &ListFilter) -> Result<Vec<QueueEntry>> {
    let mut entries = entries(ctx).await?;
    entries.retain(|entry| filter.keeps_entry(entry));
    Ok(entries)
}

/// `entries` as a text table; JSON output needs none.
pub(super) async fn table(ctx: &Context, entries: &[QueueEntry]) -> Result<ui::QueueTable> {
    if ctx.json || entries.is_empty() {
        let (header, rows) = (String::new(), Vec::new());
        return Ok(ui::QueueTable { header, rows });
    }
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let palette = Palette::stdout(ctx.json);
    Ok(ui::queue_rows(entries, &repositories, palette))
}

/// Remove `selector` from the queue when it is a waiting launch's ID; `None`
/// leaves it to workspace removal, which a starting entry belongs to.
pub(super) async fn remove(ctx: &Context, selector: &str) -> Result<Option<i32>> {
    remove_waiting(ctx, selector, |_| true).await
}

/// Remove `selector` from the queue as [`remove`] does when it names a
/// waiting launch of a new workspace; `None` leaves it to `shoal stop` of a
/// workspace, which cancels the launches waiting to open it.
pub(super) async fn remove_new(ctx: &Context, selector: &str) -> Result<Option<i32>> {
    remove_waiting(ctx, selector, |entry| entry.opens.is_none()).await
}

/// Remove `selector` from the queue as [`remove`] does when it names a
/// waiting launch that `takes` accepts.
async fn remove_waiting(
    ctx: &Context,
    selector: &str,
    takes: impl Fn(&QueueEntry) -> bool,
) -> Result<Option<i32>> {
    let found = entries(ctx).await?.into_iter().any(|entry| {
        entry.kind == QueueKind::Launch
            && entry.state == QueueState::Queued
            && entry.id == selector
            && takes(&entry)
    });
    if !found {
        return Ok(None);
    }
    let entry = queue::remove(&ctx.paths, selector.to_owned()).await?;
    ctx.emit(
        &format!("Removed {} from the agent queue", entry.name),
        &entry,
    )?;
    Ok(Some(0))
}
