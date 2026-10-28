//! `shoal resume` without `--now`: queue each stopped agent's restore, which
//! the daemon starts as the agent limits allow.
use anyhow::Result;
use serde_json::json;

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
    let output = json!({"queued": queued, "commands": commands, "failed": failed});
    ctx.show(&output, |_| {
        if !queued.is_empty() {
            outln!("{}", summary(queued.len()));
        }
    })?;
    Ok(if failed.is_empty() { 0 } else { 1 })
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
