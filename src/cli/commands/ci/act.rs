//! `shoal ci run`, `rerun` and `cancel`: start a run, or act on one run or on
//! the checks on a PR's head.
use anyhow::Result;

use crate::{
    cli::{
        args::ci::{RunArgs, Target},
        client,
        context::Context,
        ui,
    },
    forge::ci::{
        CiActed, CiAction,
        run::{StartedRun, validate_run},
        run_action::{RunActed, validate_run_reference},
    },
    protocol::CiMethod,
};

pub(super) async fn start(ctx: &Context, args: RunArgs) -> Result<i32> {
    // Refuse bad arguments before the workspace picker asks for anything.
    validate_run(args.workflow.as_deref(), args.branch.as_deref())?;
    let workspace =
        ui::select_workspace(ctx, args.workspace, ui::Fallback::CurrentDirectory).await?;
    let started: StartedRun = client::request(
        &ctx.paths,
        CiMethod::StartCiRun {
            workspace,
            workflow: args.workflow,
            branch: args.branch,
            inputs: args.inputs.into_iter().collect(),
        },
    )
    .await?;
    ctx.emit(&started_text(&started), &started)?;
    Ok(0)
}

/// Apply `action` to the run `target` names, otherwise to the checks on its
/// PR's head.
pub(super) async fn act(
    ctx: &Context,
    target: Target,
    action: CiAction,
    failed: bool,
) -> Result<i32> {
    if let Some(run) = &target.run {
        validate_run_reference(run)?;
    }
    let workspace =
        ui::select_workspace(ctx, target.workspace, ui::Fallback::CurrentDirectory).await?;
    match target.run {
        Some(run) => act_on_run(ctx, workspace, run, action, failed).await,
        None => act_on_checks(ctx, workspace, target.pr, action, target.checks).await,
    }
}

async fn act_on_run(
    ctx: &Context,
    workspace: String,
    run: String,
    action: CiAction,
    failed: bool,
) -> Result<i32> {
    let acted: RunActed = client::request(
        &ctx.paths,
        CiMethod::CiRunAction {
            workspace,
            run,
            action,
            failed,
        },
    )
    .await?;
    ctx.emit(&format!("{}: {}", outcome(action), acted.url), &acted)?;
    Ok(0)
}

/// Act on the named checks on the head of the PR `item`, by default the
/// linked one.
async fn act_on_checks(
    ctx: &Context,
    workspace: String,
    item: Option<String>,
    action: CiAction,
    checks: Vec<String>,
) -> Result<i32> {
    let acted: CiActed = client::request(
        &ctx.paths,
        CiMethod::CiAction {
            workspace,
            item,
            action,
            checks,
        },
    )
    .await?;
    ctx.emit(
        &format!(
            "{} for {}: {}",
            outcome(action),
            acted.checks.join(", "),
            acted.url
        ),
        &acted,
    )?;
    Ok(0)
}

fn outcome(action: CiAction) -> &'static str {
    match action {
        CiAction::Rerun => "Rerun started",
        CiAction::Cancel => "Cancelled",
    }
}

/// The run that started and, where the forge reports one, its URL.
fn started_text(started: &StartedRun) -> String {
    let run = match &started.workflow {
        Some(workflow) => format!("{workflow} on {}", started.branch),
        None => started.branch.clone(),
    };
    match &started.url {
        Some(url) => format!("Run started for {run}: {url}"),
        None => format!("Run started for {run}"),
    }
}
