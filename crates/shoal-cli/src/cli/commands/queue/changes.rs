//! `shoal queue start` and `cancel`: the requests that change the queue,
//! and what they report.
use anyhow::{Result, ensure};

use super::selection;
use crate::client::queue;
use crate::{cli::context::Context, model::QueueEntry, state::QueueKind};

/// `shoal queue start`: start the entry now, regardless of the limits, and
/// with `attach` attach this terminal to its agent's session.
pub(super) async fn start(ctx: &Context, entry: Option<String>, attach: bool) -> Result<i32> {
    // A process that cleared its scope token is still the workspace's.
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "start queued agents outside scoped executions"
    );
    ensure!(!attach || ctx.on_terminal(), "--attach requires a terminal");
    // A CI job runs without a session to attach.
    let offered = |queued: &QueueEntry| !attach || queued.kind != QueueKind::Job;
    let entry = selection::selected_among(ctx, entry, "Start> ", offered).await?;
    if attach {
        super::attach::require_session(ctx, &entry).await?;
    }
    let entry = queue::start(&ctx.paths, entry).await?;
    ctx.emit(
        &format!("Starting {} from the agent queue", entry.name),
        &entry,
    )?;
    if attach {
        return super::attach::started(ctx, &entry).await;
    }
    Ok(0)
}

/// `shoal queue cancel`: remove the entry; a restore's agent stays stopped.
pub(super) async fn cancel(ctx: &Context, entry: Option<String>) -> Result<i32> {
    require_scope_token(ctx)?;
    let entry = selection::selected(ctx, entry, "Cancel> ").await?;
    let entry = queue::remove(&ctx.paths, entry).await?;
    let message = match entry.kind {
        QueueKind::Launch => format!("Removed {} from the agent queue", entry.name),
        QueueKind::Restore => format!(
            "Removed the restore of {} from the agent queue; restore it with shoal resume {}",
            entry.name,
            (entry.workspace_id.as_ref()).map_or("", |id| id.as_str())
        ),
        QueueKind::Job => format!(
            "Passed over {}; the forge keeps it for other runners",
            entry.name
        ),
    };
    ctx.emit(&message, &entry)?;
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
