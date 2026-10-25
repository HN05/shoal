//! Stopping saves agent sessions and commands for `shoal resume`.
use anyhow::{Result, ensure};
use futures_util::future::join_all;
use serde_json::json;

use crate::{
    cli::{
        WorkspaceTargets,
        client::{self, request},
        context::Context,
        output::Style,
        ui,
    },
    execution::recovery,
    model::Workspace,
    protocol::{ErrorCode, Method, RemoteError},
    state::{ExecutionState, WorkspaceState},
};

pub(super) async fn run(ctx: &Context, targets: WorkspaceTargets, yes: bool) -> Result<i32> {
    if targets.all {
        return stop_all(ctx).await;
    }
    match ui::select_workspaces(ctx, targets.workspace)
        .await?
        .as_slice()
    {
        [workspace] => stop_one(ctx, workspace.clone(), yes).await,
        workspaces => {
            let workspaces = inspect_each(ctx, workspaces).await?;
            stop_each(ctx, ui::Report::new("stopped"), &workspaces, "", yes).await
        }
    }
}

async fn stop_one(ctx: &Context, workspace: String, yes: bool) -> Result<i32> {
    stop(ctx, &workspace, yes).await?;
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

/// Stop `workspace`. An execution Shoal cannot prove ended stops only once
/// the user confirms it did, or with `yes`.
async fn stop(ctx: &Context, workspace: &str, yes: bool) -> Result<()> {
    let request_stop = |acknowledge_unproven| {
        request::<()>(
            &ctx.paths,
            Method::StopWorkspace {
                workspace: workspace.to_owned(),
                acknowledge_unproven,
            },
        )
    };
    let Err(error) = request_stop(yes).await else {
        return Ok(());
    };
    let Some(unproven) = error
        .downcast_ref::<RemoteError>()
        .filter(|remote| remote.code == ErrorCode::UnprovenStop)
    else {
        return Err(error);
    };
    let action = format!(
        "{}; check that nothing of it still runs. Treat it as stopped",
        unproven.message
    );
    ensure!(ui::confirm(ctx, &action, "--yes")?, "stop canceled");
    request_stop(true).await
}

/// The workspaces `selectors` name, refusing an unknown one before any stops.
async fn inspect_each(ctx: &Context, selectors: &[String]) -> Result<Vec<Workspace>> {
    let mut workspaces = Vec::new();
    for selector in selectors {
        workspaces.push(
            client::inspect(&ctx.paths, selector.clone())
                .await?
                .workspace,
        );
    }
    Ok(workspaces)
}

/// Stop every settled workspace with a running execution.
async fn stop_all(ctx: &Context) -> Result<i32> {
    let mut report = ui::Report::new("stopped");
    let mut running = Vec::new();
    let mut unknown = false;
    for workspace in client::workspaces(&ctx.paths).await? {
        match is_running(ctx, &workspace).await {
            Ok(true) => running.push(workspace),
            Ok(false) => {}
            Err(error) => {
                report.failed(ctx, &workspace.name, &error);
                unknown = true;
            }
        }
    }
    if running.is_empty() && !unknown {
        ctx.emit(
            "No running agents or commands",
            json!({"stopped": [], "failed": []}),
        )?;
        return Ok(0);
    }
    stop_each(ctx, report, &running, " --all", false).await
}

/// Stop `workspaces` in parallel, adding to `report`; one failure does not
/// keep the others running, and an execution Shoal cannot prove ended fails
/// its workspace unless `acknowledge_unproven`. `resume` follows `shoal
/// resume` in the hint to restore them, which otherwise names each one.
async fn stop_each(
    ctx: &Context,
    mut report: ui::Report,
    workspaces: &[Workspace],
    resume: &str,
    acknowledge_unproven: bool,
) -> Result<i32> {
    let results = join_all(workspaces.iter().map(|workspace| {
        request::<()>(
            &ctx.paths,
            Method::StopWorkspace {
                workspace: workspace.id.clone(),
                acknowledge_unproven,
            },
        )
    }))
    .await;
    let mut stopped = Vec::new();
    for (workspace, result) in workspaces.iter().zip(results) {
        let name = workspace.name.as_str();
        match result {
            Ok(()) => {
                report.done(ctx, &format!("Stopped {name}"), json!(name));
                stopped.push(name);
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    if !ctx.json && !stopped.is_empty() {
        let resume = if resume.is_empty() {
            format!(" {}", stopped.join(" "))
        } else {
            resume.to_owned()
        };
        outln!("Restore with shoal resume{resume}");
    }
    report.finish(ctx)
}

/// Disconnected executions are left for `shoal stop <workspace>` rather than
/// failing every bulk stop, and preparing or removing workspaces keep their
/// lifecycle.
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
