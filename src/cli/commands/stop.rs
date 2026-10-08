//! Stopping saves agent sessions and commands for `shoal resume`.
use anyhow::Result;
use futures_util::future::join_all;
use serde_json::json;

use crate::{
    cli::{
        WorkspaceScope,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    execution::recovery,
    model::Workspace,
    protocol::Method,
    state::{ExecutionState, WorkspaceState},
};

pub(super) async fn run(ctx: &Context, scope: WorkspaceScope) -> Result<i32> {
    if scope.all {
        return stop_all(ctx).await;
    }
    let workspace = ui::select_workspace(ctx, scope.workspace, Fallback::CurrentDirectory).await?;
    request::<()>(
        &ctx.paths,
        Method::StopWorkspace {
            workspace: workspace.clone(),
        },
    )
    .await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    let saved = recovery::pending(&ctx.paths, &workspace.id)?;
    let message = if saved {
        format!(
            "Workspace processes stopped; restore with shoal resume {}",
            workspace.name
        )
    } else {
        "Workspace processes stopped".into()
    };
    ctx.emit_styled(
        Style::Success,
        &message,
        json!({"stopped": true, "saved": saved}),
    )?;
    Ok(0)
}

/// Stop every settled workspace with a running execution, in parallel; one
/// failure does not keep the others running.
async fn stop_all(ctx: &Context) -> Result<i32> {
    let mut running = Vec::new();
    for workspace in client::workspaces(&ctx.paths).await? {
        if is_running(ctx, &workspace).await? {
            running.push(workspace);
        }
    }
    let results = join_all(running.iter().map(|workspace| {
        request::<()>(
            &ctx.paths,
            Method::StopWorkspace {
                workspace: workspace.id.clone(),
            },
        )
    }))
    .await;
    let mut stopped = Vec::new();
    let mut failed = Vec::new();
    for (workspace, result) in running.iter().zip(results) {
        match result {
            Ok(()) => stopped.push(workspace.name.as_str()),
            Err(error) => failed.push((workspace.name.as_str(), format!("{error:#}"))),
        }
    }
    let output = json!({
        "stopped": stopped,
        "failed": failed
            .iter()
            .map(|(workspace, error)| json!({"workspace": workspace, "error": error}))
            .collect::<Vec<_>>(),
    });
    ctx.show(&output, |_| {
        if stopped.is_empty() && failed.is_empty() {
            println!("No running agents or commands");
        }
        let palette = Palette::stdout(ctx.json);
        for name in &stopped {
            println!(
                "{}",
                palette.paint(Style::Success, format!("Stopped {name}"))
            );
        }
        let palette = Palette::stderr(ctx.json);
        for (name, error) in &failed {
            eprintln!(
                "{}",
                palette.paint(Style::Error, format!("Not stopped {name}: {error}"))
            );
        }
        if !stopped.is_empty() {
            println!("Restore with shoal resume --all");
        }
    })?;
    Ok(if failed.is_empty() { 0 } else { 1 })
}

/// Disconnected executions are left for `shoal doctor` rather than failing
/// every bulk stop, and preparing or removing workspaces keep their lifecycle.
async fn is_running(ctx: &Context, workspace: &Workspace) -> Result<bool> {
    if !matches!(
        workspace.state,
        WorkspaceState::Ready | WorkspaceState::Failed
    ) {
        return Ok(false);
    }
    let inspection = client::inspect(&ctx.paths, workspace.id.clone()).await?;
    Ok(inspection
        .executions
        .iter()
        .any(|execution| execution.state == ExecutionState::Running))
}
