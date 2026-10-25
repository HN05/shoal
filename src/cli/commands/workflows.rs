//! `shoal forge ci`: start, rerun and cancel CI workflow runs of the
//! workspace's repository.
use anyhow::Result;

use crate::{
    cli::{ForgeCiCommand, RunArgs, RunTarget, client, context::Context, ui},
    forge::ci::{
        CiAction,
        run::{StartedRun, validate_run},
        run_action::{RunActed, validate_run_reference},
    },
    protocol::Method,
};

pub(super) async fn ci(ctx: &Context, command: ForgeCiCommand) -> Result<i32> {
    match command {
        ForgeCiCommand::Run(args) => run(ctx, args).await,
        ForgeCiCommand::Rerun { target, failed } => act(ctx, target, CiAction::Rerun, failed).await,
        ForgeCiCommand::Cancel { target } => act(ctx, target, CiAction::Cancel, false).await,
    }
}

async fn run(ctx: &Context, args: RunArgs) -> Result<i32> {
    // Refuse bad arguments before the workspace picker asks for anything.
    validate_run(args.workflow.as_deref(), args.branch.as_deref())?;
    let workspace =
        ui::select_workspace(ctx, args.workspace, ui::Fallback::CurrentDirectory).await?;
    let started: StartedRun = client::request(
        &ctx.paths,
        Method::StartCiRun {
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

async fn act(ctx: &Context, target: RunTarget, action: CiAction, failed: bool) -> Result<i32> {
    validate_run_reference(&target.run)?;
    let workspace =
        ui::select_workspace(ctx, target.workspace, ui::Fallback::CurrentDirectory).await?;
    let acted: RunActed = client::request(
        &ctx.paths,
        Method::CiRunAction {
            workspace,
            run: target.run,
            action,
            failed,
        },
    )
    .await?;
    let outcome = match action {
        CiAction::Rerun => "Rerun started",
        CiAction::Cancel => "Cancelled",
    };
    ctx.emit(&format!("{outcome}: {}", acted.url), &acted)?;
    Ok(0)
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
