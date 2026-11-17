//! `shoal ci run`, `rerun`, `cancel` and `approve`: start a run, or act on
//! one run or on the checks or runs on a PR's head.
use anyhow::Result;

use crate::client::ci;
use crate::{
    cli::{
        args::ci::{RunArgs, Subject, Target},
        commands::items::select_item_target,
        context::Context,
    },
    forge::{
        ci::{CiAction, approval::RunsApproved, run::StartedRun},
        items::reference::ItemInput,
    },
    protocol::{ItemContext, RepoTarget},
};
use shoal_forge::ci::{run::validate_run, workflow::is_workflow_url};

pub(super) async fn start(ctx: &Context, args: RunArgs) -> Result<i32> {
    // Refuse bad arguments before the workspace picker asks for anything.
    validate_run(args.workflow.as_deref(), args.branch.as_deref())?;
    let url = args.workflow.as_deref().is_some_and(is_workflow_url);
    let target = super::workflow_target(ctx, args.space, args.repo.name, url).await?;
    let follow = follow_hint(target.context.as_ref(), url);
    let inputs = args.inputs.into_iter().collect();
    let started = ci::start(&ctx.paths, target, args.workflow, args.branch, inputs).await?;
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
    match subject.run.clone() {
        Some(run) => {
            let target = subject_target(ctx, &subject).await?;
            act_on_run(ctx, target, run, action, failed).await
        }
        None => {
            let subject_target = subject_target(ctx, &subject).await?;
            act_on_checks(ctx, subject_target, subject.pr, action, target.checks).await
        }
    }
}

/// Approve the run `subject` names, otherwise every waiting run on its PR's
/// head.
pub(super) async fn approve(ctx: &Context, subject: Subject) -> Result<i32> {
    let target = subject_target(ctx, &subject).await?;
    let approved = ci::approve(&ctx.paths, target, subject.run, subject.pr).await?;
    ctx.emit(&approved_text(&approved), &approved)?;
    Ok(0)
}

/// The repository the subject's run or PR is acted on in, as
/// [`select_item_target`] finds it for the run or PR.
async fn subject_target(ctx: &Context, subject: &Subject) -> Result<RepoTarget> {
    let item = subject.run.as_ref().or(subject.pr.as_ref());
    let (space, repo) = (subject.space.clone(), subject.repo.name.clone());
    select_item_target(ctx, space, repo, item).await
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
    target: RepoTarget,
    run: ItemInput,
    action: CiAction,
    failed: bool,
) -> Result<i32> {
    let acted = ci::act_on_run(&ctx.paths, target, run, action, failed).await?;
    ctx.emit(&format!("{}: {}", outcome(action), acted.url), &acted)?;
    Ok(0)
}

/// Act on the named checks on the head of the PR `item`, by default the
/// linked one.
async fn act_on_checks(
    ctx: &Context,
    target: RepoTarget,
    item: Option<ItemInput>,
    action: CiAction,
    checks: Vec<String>,
) -> Result<i32> {
    let acted = ci::act_on_checks(&ctx.paths, target, item, action, checks).await?;
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
/// report: `ci ls` lists only the repository of the workspace or checkout,
/// while a workflow's URL may name another, and only a workspace links runs.
fn follow_hint(context: Option<&ItemContext>, by_url: bool) -> &'static str {
    match (context, by_url) {
        (
            Some(ItemContext::Space {
                repository: None, ..
            }),
            true,
        ) => "find it in the forge's web UI and follow it with shoal link ci <URL> opened",
        (
            Some(ItemContext::Space {
                repository: None, ..
            }),
            false,
        ) => "find it with shoal ci ls and follow it with shoal link ci <URL> opened",
        (Some(_), false) => "find it with shoal ci ls",
        (None, _) | (Some(_), true) => "find it in the forge's web UI",
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
