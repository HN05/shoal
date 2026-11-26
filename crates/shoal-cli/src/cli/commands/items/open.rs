//! `shoal pr open` and `shoal issue open`.
use crate::cli::ui::Acting;
use anyhow::Result;

use super::{content::read_content, context::select_open_context};
use crate::{
    cli::{args::changes::OpenPullArgs, commands::operations, context::Context, ui},
    forge::items::{
        create::{NewIssue, PullOptions},
        item::Opened,
    },
    protocol::operations::OperationSpec,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    approvals::Target,
    links::{OpenOutcome, Proposed},
};

pub(super) async fn open_pr(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    options: PullOptions,
) -> Result<i32> {
    let workspace = ui::select_workspace(
        ctx,
        workspace,
        ui::Fallback::CurrentDirectory(&opening_pull()),
    )
    .await?;
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
    let target = select_open_context(ctx, space, repository, &opening_issue()).await?;
    let outcome = (crate::client::items::open_issue(target, issue, link))
        .send(&ctx.paths)
        .await?;
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

/// An operation that pushes from the worktree on this machine. How the PR opens
/// decides nothing of which workspaces it reaches.
pub(in crate::cli::commands) fn opening_pull() -> Acting {
    Acting::request(|workspace| {
        let spec = OperationSpec::OpenPull {
            workspace,
            options: Default::default(),
        };
        crate::client::operations::start_request(spec, Default::default())
    })
}

/// The issue decides nothing of which spaces it reaches.
pub(in crate::cli::commands) fn opening_issue() -> Acting {
    super::in_context(|target| crate::client::items::open_issue(target, NewIssue::default(), false))
}
