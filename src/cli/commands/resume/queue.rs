//! `shoal resume` without `--now`, or from a caller without a terminal:
//! queue each stopped agent's restore, which the daemon starts as the agent
//! limits allow, or at once with `--now`.
use anyhow::Result;

use super::{claim_commands, reconcile_stale_execution, report_commands};
use crate::client::output::{
    batch::Failure,
    resume::{Queued, RestoredChat},
};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, ui},
    execution::recovery::Saved,
    model::{QueueEntry, Workspace},
    state::QueueState,
    terminal,
};

/// Queue the restore of every agent and last session `stopped` saves,
/// starting each at once regardless of the limits when `now`, and report
/// the commands of workspaces without one, going on after one fails, with
/// the stopped `chats` restored and the resumes already `failed`.
pub(super) async fn queue_all(
    ctx: &Context,
    stopped: Vec<(Workspace, Saved)>,
    chats: Vec<RestoredChat>,
    mut failed: Vec<Failure>,
    now: bool,
) -> Result<i32> {
    let mut queued = queue_each(ctx, stopped, now).await;
    queued.chats = chats;
    failed.append(&mut queued.failed);
    queued.failed = failed;
    ctx.show(&queued, |queued| {
        if let Some(summary) = queued.summary() {
            outln!("{summary}");
        }
    })?;
    Ok(if queued.failed.is_empty() { 0 } else { 1 })
}

/// Operations on the client type [`Queued`] that only the `shoal` command uses.
pub(crate) trait QueuedExt {
    /// What the user reads once restores are queued, `None` without any.
    fn summary(&self) -> Option<String>;
}

impl QueuedExt for Queued {
    fn summary(&self) -> Option<String> {
        let starting = (self.queued.iter()).all(|entry| entry.state == QueueState::Starting);
        (!self.queued.is_empty()).then(|| summary(self.queued.len(), starting))
    }
}

/// Queue the restore of every agent and last session `stopped` saves,
/// starting each at once when `now`, and report the commands of workspaces
/// without one that restores outside a waiting wrapper, going on after one
/// fails, which it reports.
pub(super) async fn queue_each(
    ctx: &Context,
    stopped: Vec<(Workspace, Saved)>,
    now: bool,
) -> Queued {
    let mut queued = Vec::new();
    let mut commands = Vec::new();
    let mut failed = Vec::new();
    for (workspace, mut saved) in stopped {
        if !saved.restores_outside_wrappers() {
            let _claims = claim_commands(&mut saved.commands);
            match report_commands(ctx, &workspace, &saved.commands) {
                Ok(report) => commands.push(report),
                Err(error) => failed.push(ui::failure(ctx, "resume", &workspace.name, &error)),
            }
        }
        for (execution, _) in saved.agents.iter().chain(&saved.sessions) {
            match queue(ctx, &workspace, execution, now).await {
                Ok(entry) => queued.push(entry),
                Err(error) => failed.push(ui::failure(ctx, "resume", &workspace.name, &error)),
            }
        }
    }
    Queued {
        queued,
        commands,
        chats: Vec::new(),
        failed,
    }
}

/// What the user reads once `count` restores are queued, or `starting`.
fn summary(count: usize, starting: bool) -> String {
    let agents = if count == 1 { "agent" } else { "agents" };
    let done = if starting { "Starting" } else { "Queued" };
    format!("{done} {count} {agents}; shoal queue shows them")
}

/// Queue the restore of the agent stopped as `execution` in `workspace`,
/// once an execution a daemon restart left unknown is proven ended,
/// starting it at once when `now`.
pub(super) async fn queue(
    ctx: &Context,
    workspace: &Workspace,
    execution: &str,
    now: bool,
) -> Result<QueueEntry> {
    let inspection =
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id)).await?;
    reconcile_stale_execution(ctx, &inspection, Some(execution)).await?;
    let (workspace, execution) = (workspace.id.clone(), execution.to_owned());
    crate::client::queue::restore(
        &ctx.paths,
        SpaceSelector::parse(&workspace),
        execution,
        terminal::start_environment(),
        now,
    )
    .await
}

#[cfg(test)]
mod tests;
