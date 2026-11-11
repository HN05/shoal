//! `shoal pr open` and `shoal issue open`: push and open or find a PR, or open
//! an issue, and report what the repository's approval rules made of it.
use anyhow::Result;

use super::{content::read_content, context::select_open_context};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        args::changes::OpenPullArgs,
        commands::operations::{self, Failed, Reporter},
        context::Context,
        ui,
    },
    client::{
        approvals::Target,
        links::{OpenOutcome, Proposed},
    },
    forge::items::{
        create::{NewIssue, PullOptions},
        item::Opened,
    },
    git,
    model::Workspace,
    protocol::operations::{Failure, OperationSpec, Step},
};

pub(super) async fn open_pr(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    options: PullOptions,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let opened = match publish(ctx, workspace, options).await? {
        OpenOutcome::Opened(opened) => *opened,
        OpenOutcome::Proposed(proposed) => return report_proposed(ctx, "PR", &proposed),
    };
    print_warnings(&opened);
    let outcome = if opened.created {
        "PR opened"
    } else {
        "Open PR found"
    };
    let message = if opened.linked {
        format!(
            "{outcome} and linked: {}\nRun shoal watch pr for updates and shoal done once no work is left.",
            opened.item.url
        )
    } else {
        format!("{outcome}: {}", opened.item.url)
    };
    ctx.emit(&message, &opened)?;
    Ok(0)
}

/// Push the workspace's branch and open or find its PR, in an operation
/// the daemon starts.
pub(super) async fn publish(
    ctx: &Context,
    workspace: SpaceSelector,
    options: PullOptions,
) -> Result<OpenOutcome> {
    operations::run(
        ctx,
        OperationSpec::OpenPull {
            workspace: workspace.clone(),
            options,
        },
    )
    .await
}

/// In an operation's worker: verify the worktree before anything leaves it,
/// push without forcing unless the `push` permission holds the push back for
/// the user, then let the daemon find or open the PR and link it, or record
/// what waits for the user.
pub(in crate::cli::commands) async fn push_and_open(
    ctx: &Context,
    workspace: String,
    options: PullOptions,
    reporter: &Reporter<'_>,
) -> Result<OpenOutcome> {
    let workspace =
        crate::client::workspaces::verify(&ctx.paths, SpaceSelector::parse(&workspace), true)
            .await?;
    workspace.require_branch("shoal pr open")?;
    crate::client::reviews::check_pr_gate(&ctx.paths, SpaceSelector::id(&workspace.id)).await?;
    let held =
        crate::client::approvals::push_held(&ctx.paths, SpaceSelector::id(&workspace.id)).await?;
    if !held {
        reporter.step(Step::PushStarted).await?;
        push(ctx, &workspace).await?;
        reporter.step(Step::Pushed).await?;
    }
    crate::client::items::open_pull(&ctx.paths, SpaceSelector::id(&workspace.id), options, held)
        .await
}

/// Push to the branch's push remote as a tracked command under the agent
/// account, with the user's Git configuration and hooks.
async fn push(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let branch = workspace.require_branch("shoal pr open")?;
    let remote = git::push_remote(&workspace.path, branch).await?;
    let code = crate::execution::publish(
        &ctx.paths,
        SpaceSelector::Id(workspace.id.clone()),
        git::push_command(&remote, branch),
    )
    .await?;
    if code != 0 {
        let message = format!("git push of {branch} to {remote} failed (exit {code})");
        return Err(Failed(Failure::PushFailed { message }).into());
    }
    Ok(())
}

pub(super) async fn open_issue(
    ctx: &Context,
    space: Option<SpaceSelector>,
    issue: NewIssue,
    repository: Option<String>,
    link: bool,
) -> Result<i32> {
    let (context, repository) = select_open_context(ctx, space, repository).await?;
    let outcome =
        crate::client::items::open_issue(&ctx.paths, context, issue, repository, link).await?;
    let opened = match outcome {
        OpenOutcome::Opened(opened) => *opened,
        OpenOutcome::Proposed(proposed) => return report_proposed(ctx, "Issue", &proposed),
    };
    print_warnings(&opened);
    let linked = if opened.linked { " and linked" } else { "" };
    ctx.emit(
        &format!("Issue opened{linked}: {}", opened.item.url),
        &opened,
    )?;
    Ok(0)
}

/// The repository requires approval of the `item` or of its push: the
/// agent continues its work and hears the decision as a message.
pub(super) fn report_proposed(ctx: &Context, item: &str, proposed: &Proposed) -> Result<i32> {
    let request = &proposed.proposed;
    let (kind, decides) = match (&request.target, request.pending_push()) {
        (Target::Push, _) => ("Push", "pushes it"),
        (_, Some(_)) => (item, "pushes the branch and opens it"),
        (_, None) => (item, "opens it"),
    };
    ctx.emit(
        &format!(
            "{kind} proposed for approval {}: the user decides whether Shoal {decides}, and the decision arrives as a message.",
            request.number
        ),
        proposed,
    )?;
    Ok(0)
}

pub(super) fn print_warnings(opened: &Opened) {
    for warning in &opened.warnings {
        eprintln!("{warning}");
    }
}

/// What a new PR is opened with, its files checked and its body read.
pub(super) fn pull_options(pull: OpenPullArgs) -> Result<PullOptions> {
    let (body, attachments) = read_content(pull.body, pull.files)?;
    Ok(PullOptions {
        title: pull.title,
        body,
        base: pull.base,
        draft: (pull.draft || pull.no_draft).then_some(pull.draft),
        labels: pull.labels,
        reviewers: pull.reviewers,
        assignees: pull.assignees,
        milestone: pull.milestone,
        attachments,
    })
}
