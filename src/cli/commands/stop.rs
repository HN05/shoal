//! Stopping saves agent sessions and commands for `shoal resume`.
use anyhow::{Result, ensure};
use futures_util::future::join_all;
use serde_json::json;

use crate::{
    cli::{
        args::WorkspaceTargets,
        client::{self, request},
        context::Context,
        output::Style,
        ui,
    },
    execution::recovery,
    model::Workspace,
    protocol::{ErrorCode, RemoteError, RemovalMethod},
    state::WorkspaceState,
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
            stop_each(
                ctx,
                ui::Report::new("stopped", "stop"),
                &workspaces,
                "",
                yes,
            )
            .await
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
            RemovalMethod::StopWorkspace {
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

/// The workspaces `selectors` name, refusing them all before any stops when
/// one is unknown or in another lifecycle operation, which stopping refuses.
async fn inspect_each(ctx: &Context, selectors: &[String]) -> Result<Vec<Workspace>> {
    let mut workspaces = Vec::new();
    let mut refusals = Vec::new();
    for selector in selectors {
        let workspace = match client::inspect(&ctx.paths, selector.clone()).await {
            Ok(inspection) => inspection.workspace,
            Err(error) => {
                refusals.push(format!("{selector}: {error:#}"));
                continue;
            }
        };
        let state = workspace.state;
        if matches!(state, WorkspaceState::Ready | WorkspaceState::Failed) {
            workspaces.push(workspace);
        } else {
            refusals.push(format!(
                "{}: workspace is {state}; wait for that operation to finish",
                workspace.name
            ));
        }
    }
    ensure!(
        refusals.is_empty(),
        "cannot stop these workspaces, so none was stopped:\n  {}",
        refusals.join("\n  ")
    );
    Ok(workspaces)
}

/// Stop every settled workspace with a running execution or an agent whose
/// restore is queued or requested, which stopping cancels; the daemon selects
/// them while no restore can move between the queue and its agent.
async fn stop_all(ctx: &Context) -> Result<i32> {
    let report = ui::Report::new("stopped", "stop");
    let running = request::<Vec<Workspace>>(&ctx.paths, RemovalMethod::StopTargets).await?;
    if running.is_empty() {
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
            RemovalMethod::StopWorkspace {
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
    let summary = match (stopped.is_empty(), resume.is_empty()) {
        (true, _) => String::new(),
        (false, true) => format!("Restore with shoal resume {}", stopped.join(" ")),
        (false, false) => format!("Restore with shoal resume{resume}"),
    };
    report.finish(ctx, &summary)
}
