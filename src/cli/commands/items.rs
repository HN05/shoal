//! `shoal pr` and `shoal issue`: ask the daemon to open or change an item of
//! the workspace's repository and report it as the forge now does.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        BodyArgs, EditArgs, IssueCommand, ItemSelector, PrCommand, client, context::Context, ui,
    },
    forge::{
        action::{Action, Edit},
        create::{NewIssue, PullOptions},
        item::{Item, Opened},
        link::ItemKind,
    },
    git,
    model::Workspace,
    protocol::Method,
    tools::Tool,
};

pub(super) async fn pr(ctx: &Context, command: PrCommand) -> Result<i32> {
    let (item, action, outcome) = match command {
        PrCommand::Open {
            workspace,
            title,
            body,
            base,
            draft,
            labels,
            reviewers,
        } => {
            let options = PullOptions {
                title,
                body: read_body(body)?,
                base,
                draft,
                labels,
                reviewers,
            };
            return open_pr(ctx, workspace, options).await;
        }
        PrCommand::Edit {
            item,
            edit,
            base,
            add_reviewers,
            remove_reviewers,
            draft,
            no_draft,
        } => {
            let edit = Edit {
                base,
                add_reviewers,
                remove_reviewers,
                draft: (draft || no_draft).then_some(draft),
                ..edit_fields(edit)?
            };
            (item, Action::Edit(edit), "PR updated")
        }
        PrCommand::Comment { item, body } => (item, comment(body)?, "Commented on PR"),
        PrCommand::Close { item } => (item, Action::Close, "PR closed"),
        PrCommand::Reopen { item } => (item, Action::Reopen, "PR reopened"),
        PrCommand::Merge {
            item,
            method,
            delete_branch,
        } => (
            item,
            Action::Merge {
                method,
                delete_branch,
            },
            "PR merged",
        ),
    };
    act(ctx, item, ItemKind::Pr, action, outcome).await
}

pub(super) async fn issue(ctx: &Context, command: IssueCommand) -> Result<i32> {
    let (item, action, outcome) = match command {
        IssueCommand::Open {
            workspace,
            title,
            body,
            labels,
            link,
        } => {
            let issue = NewIssue {
                title,
                body: read_body(body)?.unwrap_or_default(),
                labels,
            };
            return open_issue(ctx, workspace, issue, link).await;
        }
        IssueCommand::Edit { item, edit } => {
            (item, Action::Edit(edit_fields(edit)?), "Issue updated")
        }
        IssueCommand::Comment { item, body } => (item, comment(body)?, "Commented on issue"),
        IssueCommand::Close { item } => (item, Action::Close, "Issue closed"),
        IssueCommand::Reopen { item } => (item, Action::Reopen, "Issue reopened"),
    };
    act(ctx, item, ItemKind::Issue, action, outcome).await
}

async fn act(
    ctx: &Context,
    selector: ItemSelector,
    kind: ItemKind,
    action: Action,
    outcome: &str,
) -> Result<i32> {
    action.validate(kind)?;
    let workspace =
        ui::select_workspace(ctx, selector.workspace, ui::Fallback::CurrentDirectory).await?;
    let item: Item = client::request(
        &ctx.paths,
        Method::ItemAction {
            workspace,
            kind,
            item: selector.item,
            action,
        },
    )
    .await?;
    ctx.emit(&format!("{outcome}: {}", item.url), &item)?;
    Ok(0)
}

/// Verify the worktree before anything leaves it, push without forcing, then
/// let the daemon find or open the PR and link it.
async fn open_pr(ctx: &Context, workspace: Option<String>, options: PullOptions) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let workspace: Workspace = client::request(
        &ctx.paths,
        Method::VerifyWorkspace {
            workspace,
            on_branch: true,
        },
    )
    .await?;
    push(ctx, &workspace).await?;
    let opened: Opened = client::request(
        &ctx.paths,
        Method::OpenPull {
            workspace: workspace.id,
            options,
        },
    )
    .await?;
    let outcome = if opened.created {
        "PR opened"
    } else {
        "Open PR found"
    };
    ctx.emit(
        &format!(
            "{outcome} and linked: {}\nRun shoal watch pr for updates and shoal done once the assignment is finished.",
            opened.item.url
        ),
        &opened,
    )?;
    Ok(0)
}

/// Push as a tracked command under the agent account, with the user's Git
/// configuration and hooks.
async fn push(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let branch = git::local_ref(&workspace.branch);
    let command = [
        Tool::Git.program(),
        "push",
        "--set-upstream",
        "--",
        "origin",
        &format!("{branch}:{branch}"),
    ];
    let code = crate::execution::publish(
        &ctx.paths,
        workspace.id.clone(),
        command.map(OsString::from).to_vec(),
    )
    .await?;
    ensure!(
        code == 0,
        "git push of {} to origin failed (exit {code})",
        workspace.branch
    );
    Ok(())
}

async fn open_issue(
    ctx: &Context,
    workspace: Option<String>,
    issue: NewIssue,
    link: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let opened: Opened = client::request(
        &ctx.paths,
        Method::OpenIssue {
            workspace,
            issue,
            link,
        },
    )
    .await?;
    let linked = if opened.linked { " and linked" } else { "" };
    ctx.emit(
        &format!("Issue opened{linked}: {}", opened.item.url),
        &opened,
    )?;
    Ok(0)
}

fn edit_fields(edit: EditArgs) -> Result<Edit> {
    Ok(Edit {
        title: edit.title,
        body: read_body(edit.body)?,
        add_labels: edit.add_labels,
        remove_labels: edit.remove_labels,
        ..Edit::default()
    })
}

fn comment(body: BodyArgs) -> Result<Action> {
    let body = read_body(body)?.context("a comment needs --body or --body-file")?;
    Ok(Action::Comment { body })
}

fn read_body(body: BodyArgs) -> Result<Option<String>> {
    match body.body_file {
        Some(path) => std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))
            .map(Some),
        None => Ok(body.body),
    }
}
