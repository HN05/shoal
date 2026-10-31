//! `shoal ci run`, `rerun`, `cancel` and `approve`: start a run, or act on
//! one run or on the checks or runs on a PR's head.
use anyhow::Result;

use crate::{
    cli::{
        args::ci::{RunArgs, Subject, Target},
        client,
        commands::items::select_item_context,
        context::Context,
    },
    forge::ci::{
        CiActed, CiAction,
        approval::RunsApproved,
        run::{StartedRun, validate_run},
        run_action::{RunActed, validate_run_reference},
        workflow::is_workflow_url,
    },
    protocol::{CiMethod, ItemContext},
};

pub(super) async fn start(ctx: &Context, args: RunArgs) -> Result<i32> {
    // Refuse bad arguments before the workspace picker asks for anything.
    validate_run(args.workflow.as_deref(), args.branch.as_deref())?;
    let workflow = args.workflow.as_deref();
    let context = super::input_context(ctx, args.workspace, args.repo.name, workflow).await?;
    let follow = follow_hint(context.as_ref(), workflow.is_some_and(is_workflow_url));
    let started: StartedRun = client::request(
        &ctx.paths,
        CiMethod::StartCiRun {
            context,
            workflow: args.workflow,
            branch: args.branch,
            inputs: args.inputs.into_iter().collect(),
        },
    )
    .await?;
    for warning in &started.warnings {
        eprintln!("{warning}");
    }
    ctx.emit(&started_text(&started, follow), &started)?;
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
    match subject.run.clone() {
        Some(run) => {
            let (workspace, repo) = (subject.workspace.clone(), subject.repo.name.clone());
            let context = super::input_context(ctx, workspace, repo, Some(&run)).await?;
            act_on_run(ctx, context, run, action, failed).await
        }
        None => {
            let context = subject_context(ctx, &subject).await?;
            act_on_checks(ctx, context, subject.pr, action, target.checks).await
        }
    }
}

/// Approve the run `subject` names, otherwise every waiting run on its PR's
/// head.
pub(super) async fn approve(ctx: &Context, subject: Subject) -> Result<i32> {
    if let Some(run) = &subject.run {
        validate_run_reference(run)?;
    }
    let context = subject_context(ctx, &subject).await?;
    let approved: RunsApproved = client::request(
        &ctx.paths,
        CiMethod::ApproveCiRuns {
            context,
            run: subject.run,
            pr: subject.pr,
        },
    )
    .await?;
    ctx.emit(&approved_text(&approved), &approved)?;
    Ok(0)
}

/// Where the subject's run or PR is acted on: the named or current
/// workspace, or the checkout of the registered repository `--repo` names or
/// the current directory or the PR's URL is in.
async fn subject_context(ctx: &Context, subject: &Subject) -> Result<ItemContext> {
    select_item_context(
        ctx,
        subject.workspace.clone(),
        subject.repo.name.clone(),
        subject.pr.as_deref(),
    )
    .await
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
    context: Option<ItemContext>,
    run: String,
    action: CiAction,
    failed: bool,
) -> Result<i32> {
    let acted: RunActed = client::request(
        &ctx.paths,
        CiMethod::CiRunAction {
            context,
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
    context: ItemContext,
    item: Option<String>,
    action: CiAction,
    checks: Vec<String>,
) -> Result<i32> {
    let acted: CiActed = client::request(
        &ctx.paths,
        CiMethod::CiAction {
            context,
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

/// How to find and follow a started run whose URL the forge does not
/// report: `ci list` lists only the repository of the workspace or checkout,
/// while a workflow's URL may name another, and only a workspace links runs.
fn follow_hint(context: Option<&ItemContext>, by_url: bool) -> &'static str {
    match (context, by_url) {
        (None, _) | (Some(ItemContext::Checkout(_)), true) => "find it in the forge's web UI",
        (Some(ItemContext::Checkout(_)), false) => "find it with shoal ci list",
        (Some(ItemContext::Workspace(_)), true) => {
            "find it in the forge's web UI and follow it with shoal link ci <URL> opened"
        }
        (Some(ItemContext::Workspace(_)), false) => {
            "find it with shoal ci list and follow it with shoal link ci <URL> opened"
        }
    }
}

/// The run that started, its URL where the forge reports one, and whether
/// the workspace links and watches it, or otherwise how to `follow` it.
fn started_text(started: &StartedRun, follow: &str) -> String {
    let run = match &started.workflow {
        Some(workflow) => format!("{workflow} on {}", started.branch),
        None => started.branch.clone(),
    };
    match &started.url {
        Some(url) if started.linked => format!(
            "Run started for {run}: {url}\nLinked as opened; shoal watch reports when it finishes."
        ),
        Some(url) => format!("Run started for {run}: {url}"),
        None => format!("Run started for {run}; {follow}"),
    }
}
