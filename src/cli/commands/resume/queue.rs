//! `shoal resume` without `--now`: queue each stopped agent's restore, which
//! the daemon starts as the agent limits allow.
use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};

use super::{claim_commands, reconcile_stale_execution, report_commands};
use crate::{
    cli::{client, context::Context, terminal, ui},
    execution::recovery::Saved,
    model::{QueueEntry, Workspace},
    protocol::QueueMethod,
};

/// Queue the restore of every agent and last session `stopped` saves, and
/// report the commands of workspaces without one, going on after one fails.
pub(super) async fn queue_all(ctx: &Context, stopped: Vec<(Workspace, Saved)>) -> Result<i32> {
    if stopped.is_empty() {
        ctx.emit(
            "No stopped agents or commands",
            json!({"queued": [], "commands": []}),
        )?;
        return Ok(0);
    }
    let queued = queue_each(ctx, stopped).await;
    ctx.show(&queued, |queued| {
        if let Some(summary) = queued.summary() {
            outln!("{summary}");
        }
    })?;
    Ok(if queued.failed.is_empty() { 0 } else { 1 })
}

/// What [`queue_each`] did: the restores it queued, the commands it reported
/// and the workspaces it failed for.
#[derive(Serialize)]
pub(in crate::cli::commands) struct Queued {
    queued: Vec<QueueEntry>,
    commands: Vec<Value>,
    failed: Vec<Value>,
}

impl Queued {
    /// What the user reads once restores are queued, `None` without any.
    pub(in crate::cli::commands) fn summary(&self) -> Option<String> {
        (!self.queued.is_empty()).then(|| summary(self.queued.len()))
    }
}

/// Queue the restore of every agent and last session `stopped` saves, and
/// report the commands of workspaces without one, going on after one fails,
/// which it reports.
pub(super) async fn queue_each(ctx: &Context, stopped: Vec<(Workspace, Saved)>) -> Queued {
    let mut queued = Vec::new();
    let mut commands = Vec::new();
    let mut failed = Vec::new();
    for (workspace, mut saved) in stopped {
        if saved.agents.is_empty() && saved.sessions.is_empty() {
            let _claims = claim_commands(&mut saved.commands);
            match report_commands(ctx, &workspace, &saved.commands) {
                Ok(report) => commands.push(report),
                Err(error) => failed.push(ui::failure(ctx, "resume", &workspace.name, &error)),
            }
        }
        for (execution, _) in saved.agents.iter().chain(&saved.sessions) {
            match queue(ctx, &workspace, execution).await {
                Ok(entry) => queued.push(entry),
                Err(error) => failed.push(ui::failure(ctx, "resume", &workspace.name, &error)),
            }
        }
    }
    Queued {
        queued,
        commands,
        failed,
    }
}

/// What the user reads once `count` restores are queued.
fn summary(count: usize) -> String {
    let agents = if count == 1 { "agent" } else { "agents" };
    format!("Queued {count} {agents}; shoal queue shows them")
}

/// Queue the restore of the agent stopped as `execution` in `workspace`,
/// once an execution a daemon restart left unknown is proven ended.
async fn queue(ctx: &Context, workspace: &Workspace, execution: &str) -> Result<QueueEntry> {
    let inspection = client::inspect(&ctx.paths, workspace.id.clone()).await?;
    reconcile_stale_execution(ctx, &inspection, Some(execution)).await?;
    client::request(
        &ctx.paths,
        QueueMethod::QueueRestore {
            workspace: workspace.id.clone(),
            execution: execution.to_owned(),
            env: terminal::start_environment(),
        },
    )
    .await
}

#[cfg(test)]
mod tests;
