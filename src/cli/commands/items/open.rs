//! `shoal pr open` and `shoal issue open`: push and open or find a PR, or open
//! an issue, and report what the repository's approval rules made of it.
use anyhow::{Result, ensure};

use super::content::read_content;
use crate::{
    cli::{args::changes::OpenPullArgs, client, context::Context, ui},
    daemon::links::proposals::{OpenOutcome, Proposed},
    forge::items::{
        create::{NewIssue, PullOptions},
        item::Opened,
    },
    git,
    model::Workspace,
    protocol::{ItemMethod, WorkspaceMethod},
};

/// Verify the worktree before anything leaves it, push without forcing, then
/// let the daemon find or open the PR and link it.
pub(super) async fn open_pr(
    ctx: &Context,
    workspace: Option<String>,
    options: PullOptions,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let workspace: Workspace = client::request(
        &ctx.paths,
        WorkspaceMethod::VerifyWorkspace {
            workspace,
            on_branch: true,
        },
    )
    .await?;
    push(ctx, &workspace).await?;
    let outcome: OpenOutcome = client::request(
        &ctx.paths,
        ItemMethod::OpenPull {
            workspace: workspace.id,
            options,
        },
    )
    .await?;
    let opened = match outcome {
        OpenOutcome::Opened(opened) => opened,
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
            "{outcome} and linked: {}\nRun shoal watch pr for updates and shoal done once the assignment is finished.",
            opened.item.url
        )
    } else {
        format!("{outcome}: {}", opened.item.url)
    };
    ctx.emit(&message, &opened)?;
    Ok(0)
}

/// Push to the branch's push remote as a tracked command under the agent
/// account, with the user's Git configuration and hooks.
async fn push(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let remote = git::push_remote(&workspace.path, &workspace.branch).await?;
    let code = crate::execution::publish(
        &ctx.paths,
        workspace.id.clone(),
        git::push_command(&remote, &workspace.branch),
    )
    .await?;
    ensure!(
        code == 0,
        "git push of {} to {remote} failed (exit {code})",
        workspace.branch
    );
    Ok(())
}

pub(super) async fn open_issue(
    ctx: &Context,
    workspace: Option<String>,
    issue: NewIssue,
    repository: Option<String>,
    link: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let outcome: OpenOutcome = client::request(
        &ctx.paths,
        ItemMethod::OpenIssue {
            workspace,
            issue,
            repository,
            link,
        },
    )
    .await?;
    let opened = match outcome {
        OpenOutcome::Opened(opened) => opened,
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

/// The repository requires approval: the agent continues its work and
/// hears the decision as a message.
fn report_proposed(ctx: &Context, kind: &str, proposed: &Proposed) -> Result<i32> {
    ctx.emit(
        &format!(
            "{kind} proposed for approval {}: the user decides whether Shoal opens it, and the decision arrives as a message.",
            proposed.proposed.id
        ),
        proposed,
    )?;
    Ok(0)
}

fn print_warnings(opened: &Opened) {
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
        attachments,
    })
}
