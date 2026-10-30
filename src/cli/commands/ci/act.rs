//! `shoal ci run`, `rerun`, `cancel` and `approve`: start a run, or act on
//! one run or on the checks or runs on a PR's head.
use anyhow::Result;

use crate::{
    cli::{
        args::ci::{RunArgs, Subject, Target},
        client,
        context::Context,
        ui,
    },
    forge::ci::{
        CiActed, CiAction,
        approval::RunsApproved,
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
    for warning in &started.warnings {
        eprintln!("{warning}");
    }
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
    let subject = target.subject;
    if let Some(run) = &subject.run {
        validate_run_reference(run)?;
    }
    let workspace =
        ui::select_workspace(ctx, subject.workspace, ui::Fallback::CurrentDirectory).await?;
    match subject.run {
        Some(run) => act_on_run(ctx, workspace, run, action, failed).await,
        None => act_on_checks(ctx, workspace, subject.pr, action, target.checks).await,
    }
}

/// Approve the run `subject` names, otherwise every waiting run on its PR's
/// head.
pub(super) async fn approve(ctx: &Context, subject: Subject) -> Result<i32> {
    if let Some(run) = &subject.run {
        validate_run_reference(run)?;
    }
    let workspace =
        ui::select_workspace(ctx, subject.workspace, ui::Fallback::CurrentDirectory).await?;
    let approved: RunsApproved = client::request(
        &ctx.paths,
        CiMethod::ApproveCiRuns {
            workspace,
            run: subject.run,
            pr: subject.pr,
        },
    )
    .await?;
    ctx.emit(&approved_text(&approved), &approved)?;
    Ok(0)
}

/// One line per approved run.
fn approved_text(approved: &RunsApproved) -> String {
    approved
        .approved
        .iter()
        .map(|url| format!("Approved: {url}"))
        .collect::<Vec<_>>()
        .join("\n")
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

/// The run that started, its URL where the forge reports one, and whether
/// the workspace links and watches it.
fn started_text(started: &StartedRun) -> String {
    let run = match &started.workflow {
        Some(workflow) => format!("{workflow} on {}", started.branch),
        None => started.branch.clone(),
    };
    match &started.url {
        Some(url) if started.linked => format!(
            "Run started for {run}: {url}\nLinked as opened; shoal watch reports when it finishes."
        ),
        Some(url) => format!("Run started for {run}: {url}"),
        None => format!(
            "Run started for {run}; find it with shoal ci list and follow it with \
             shoal link ci <URL> opened"
        ),
    }
}
