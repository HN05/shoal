//! `shoal queue start`, `move` and `cancel`: the requests that change the
//! queue, and what they report.
use anyhow::{Result, ensure};

use super::selection;
use crate::{
    cli::{client::request, context::Context},
    model::{MovedEntry, QueueEntry, QueuePlace},
    protocol::QueueMethod,
    state::QueueKind,
};

/// `shoal queue start`: start the entry now, regardless of the limits.
pub(super) async fn start(ctx: &Context, entry: Option<String>) -> Result<i32> {
    // A process that cleared its scope token is still the workspace's.
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "start queued agents outside scoped executions"
    );
    let entry = selection::selected(ctx, entry, "Start> ").await?;
    let entry: QueueEntry = request(&ctx.paths, QueueMethod::QueueStart { entry }).await?;
    ctx.emit(
        &format!("Starting {} from the agent queue", entry.name),
        &entry,
    )?;
    Ok(0)
}

/// `shoal queue cancel`: remove the entry; a restore's agent stays stopped.
pub(super) async fn cancel(ctx: &Context, entry: Option<String>) -> Result<i32> {
    require_scope_token(ctx)?;
    let entry = selection::selected(ctx, entry, "Cancel> ").await?;
    let entry: QueueEntry = request(&ctx.paths, QueueMethod::QueueRemove { entry }).await?;
    let message = match entry.kind {
        QueueKind::Launch => format!("Removed {} from the agent queue", entry.name),
        QueueKind::Restore => format!(
            "Removed the restore of {0} from the agent queue; restore it with shoal resume {0}",
            entry.name
        ),
    };
    ctx.emit(&message, &entry)?;
    Ok(0)
}

/// `shoal queue move`: place the entry, or a running agent's restore, at
/// `position`, or in front of the entry chosen on a terminal.
pub(super) async fn move_entry(
    ctx: &Context,
    entry: Option<String>,
    position: Option<QueuePlace>,
) -> Result<i32> {
    require_scope_token(ctx)?;
    let entry = selection::selected(ctx, entry, "Move> ").await?;
    let place = match position {
        Some(place) => place,
        None => selection::pick_place(ctx, &entry).await?,
    };
    let moved: MovedEntry = request(&ctx.paths, QueueMethod::QueueMove { entry, place }).await?;
    let place = ordinal(moved.entry.position.unwrap_or_default());
    let message = if moved.stopped {
        format!(
            "Stopped {}; its restore is {place} in the queue",
            moved.entry.name
        )
    } else {
        format!("Moved {}; it is {place} in the queue", moved.entry.name)
    };
    ctx.emit(&message, &moved)?;
    Ok(0)
}

/// Refuse a workspace process that cleared its scope token, which the
/// daemon would take for the user; with the token it confines the change.
fn require_scope_token(ctx: &Context) -> Result<()> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state) || crate::env::is_scoped(),
        "workspace processes change the agent queue only with their scope token"
    );
    Ok(())
}

/// `n` as an English ordinal: 1st, 2nd, 3rd, 4th, 11th, 21st.
fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests;
