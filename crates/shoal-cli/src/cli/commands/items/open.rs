//! `shoal pr open` and `shoal issue open`: push and open or find a PR, or open
//! an issue, and report what the repository's approval rules made of it.
use anyhow::Result;

use super::{content::read_content, context::select_open_context};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{args::changes::OpenPullArgs, commands::operations, context::Context, ui},
    client::{
        approvals::Target,
        links::{OpenOutcome, Proposed},
    },
    forge::items::{
        create::{NewIssue, PullOptions},
        item::Opened,
    },
    protocol::operations::OperationSpec,
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
