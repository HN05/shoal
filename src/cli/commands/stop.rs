//! Stopping keeps agent sessions and commands for `shoal resume`; agents
//! stay down until it continues them.
use anyhow::{Context as _, Result, ensure};
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

pub(super) async fn run(
    ctx: &Context,
    targets: WorkspaceTargets,
    yes: bool,
    skip_children: bool,
) -> Result<i32> {
    if targets.all {
        return stop_all(ctx).await;
    }
    let mut selected = ui::select_workspaces(ctx, targets.workspace).await?;
    if !skip_children {
        selected = with_children(ctx, selected).await?;
    }
    if crate::env::inherits_scope(&ctx.paths.state) {
        ensure_below(ctx, &selected).await?;
    }
    match selected.as_slice() {
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
    let saved = !recovery::Saved::load(&ctx.paths, &workspace.id)?.is_empty();
    let message = if saved {
        format!(
            "Workspace processes stopped. {}",
            continue_hint(&workspace.name)
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

/// The IDs of the workspaces `selectors` name, each followed by the
/// workspaces below it, which a stop of it stops too: their agents work for
/// it.
async fn with_children(ctx: &Context, selectors: Vec<String>) -> Result<Vec<String>> {
    let workspaces = client::workspaces(&ctx.paths).await?;
    let mut ids = Vec::new();
    for selector in selectors {
        ids.push(client::inspect(&ctx.paths, selector).await?.workspace.id);
    }
    Ok(with_descendants(ids, &workspaces))
}

/// `ids` once each, followed by every workspace of `workspaces` below them.
fn with_descendants(ids: Vec<String>, workspaces: &[Workspace]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for id in ids {
        if !found.contains(&id) {
            found.push(id);
        }
    }
    let mut next = 0;
    while let Some(parent) = found.get(next).cloned() {
        for workspace in workspaces {
            let child = workspace
                .parent_workspace
                .as_ref()
                .is_some_and(|reference| reference.id == parent);
            if child && !found.contains(&workspace.id) {
                found.push(workspace.id.clone());
            }
        }
        next += 1;
    }
    found
}

/// Refuse a stop in a workspace process that `selected` includes its own
/// workspace in. The daemon refuses each workspace not below the caller's,
/// which needs the scope token; refusing its own here, before any stops,
/// keeps the stop from taking the workspaces below it alone.
pub(super) async fn ensure_below(ctx: &Context, selected: &[String]) -> Result<()> {
    const REFUSAL: &str = "workspace processes stop only the workspaces below theirs; finish their own with shoal done";
    let own = std::env::var(crate::env::WORKSPACE_ID).context(REFUSAL)?;
    ensure!(crate::env::is_scoped(), "{REFUSAL}");
    for selector in selected {
        let id = client::inspect(&ctx.paths, selector.clone())
            .await?
            .workspace
            .id;
        ensure!(id != own, "{REFUSAL}");
    }
    Ok(())
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
    stop_each(ctx, report, &running, "--all", false).await
}

/// Stop `workspaces` in parallel, adding to `report`; one failure does not
/// keep the others running, and an execution Shoal cannot prove ended fails
/// its workspace unless `acknowledge_unproven`. `resume` is what the hint
/// to continue them passes `shoal resume`; empty, it names each one.
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
    let mut resumable = Vec::new();
    for (workspace, result) in workspaces.iter().zip(results) {
        let name = workspace.name.as_str();
        match result {
            Ok(()) => {
                report.done(ctx, &format!("Stopped {name}"), json!(name));
                if has_saved_work(ctx, workspace) {
                    resumable.push(name);
                }
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    let summary = match (resumable.is_empty(), resume.is_empty()) {
        (true, _) => String::new(),
        (false, true) => continue_hint(&resumable.join(" ")),
        (false, false) => continue_hint(resume),
    };
    report.finish(ctx, &summary)
}

/// Whether `workspace` saved work for `shoal resume`, which refuses to
/// resume a workspace without any.
fn has_saved_work(ctx: &Context, workspace: &Workspace) -> bool {
    recovery::Saved::load(&ctx.paths, &workspace.id).is_ok_and(|saved| !saved.is_empty())
}

/// How to continue what `shoal stop` stopped in `targets`, the arguments of
/// `shoal resume`, and how to take agents down only briefly instead.
fn continue_hint(targets: &str) -> String {
    format!(
        "Continue with shoal resume {targets}. To restart agents without stopping them, use shoal restart"
    )
}
