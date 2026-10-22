//! `shoal pr`, `shoal ci` and `shoal issue`: ask the daemon to open or change
//! an item of the workspace's repository and report it as the forge now does.
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        BodyArgs, CiCommand, EditArgs, IssueCommand, ItemSelector, PrCommand, client,
        context::Context, ui,
    },
    forge::{
        ForgeRepo,
        action::{Action, Edit, ThreadChange},
        ci::{CiActed, CiAction},
        create::{NewIssue, PullOptions},
        item::{Item, Opened},
        link::{ItemKind, Selection},
        view::Selected,
    },
    git,
    model::Workspace,
    protocol::Method,
};

pub(super) async fn pr(ctx: &Context, command: PrCommand) -> Result<i32> {
    let (item, action, outcome) = match command {
        PrCommand::Open {
            workspace,
            title,
            body,
            base,
            draft,
            no_draft,
            labels,
            reviewers,
        } => {
            let options = PullOptions {
                title,
                body: read_body(body)?,
                base,
                draft: (draft || no_draft).then_some(draft),
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
        PrCommand::Reply {
            comment,
            item,
            body,
        } => {
            let reply = read_body(body)?.context("a reply needs --body or --body-file")?;
            (
                item,
                thread(comment, Some(reply), None),
                "Replied in thread",
            )
        }
        PrCommand::Resolve {
            comment,
            item,
            body,
            confirmation,
        } => {
            let reply = read_body(body)?;
            if reply.is_none() && !confirmation.yes {
                confirm_unanswered(ctx, &item, comment).await?;
            }
            (item, thread(comment, reply, Some(true)), "Thread resolved")
        }
        PrCommand::Unresolve { comment, item } => (
            item,
            thread(comment, None, Some(false)),
            "Thread unresolved",
        ),
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

pub(super) async fn ci(ctx: &Context, command: CiCommand) -> Result<i32> {
    let (action, checks) = match command {
        CiCommand::Rerun { checks } => (CiAction::Rerun, checks),
        CiCommand::Cancel { checks } => (CiAction::Cancel, checks),
    };
    let workspace =
        ui::select_workspace(ctx, checks.item.workspace, ui::Fallback::CurrentDirectory).await?;
    let acted: CiActed = client::request(
        &ctx.paths,
        Method::CiAction {
            workspace,
            item: checks.item.item,
            action,
            checks: checks.checks,
        },
    )
    .await?;
    let outcome = match action {
        CiAction::Rerun => "Rerun started",
        CiAction::Cancel => "Cancelled",
    };
    ctx.emit(
        &format!("{outcome} for {}: {}", acted.checks.join(", "), acted.url),
        &acted,
    )?;
    Ok(0)
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

/// Push as a tracked command under the agent account, with the user's Git
/// configuration and hooks.
async fn push(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let code = crate::execution::publish(
        &ctx.paths,
        workspace.id.clone(),
        git::push_command(&workspace.branch),
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
    print_warnings(&opened);
    let linked = if opened.linked { " and linked" } else { "" };
    ctx.emit(
        &format!("Issue opened{linked}: {}", opened.item.url),
        &opened,
    )?;
    Ok(0)
}

fn print_warnings(opened: &Opened) {
    for warning in &opened.warnings {
        eprintln!("{warning}");
    }
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

fn thread(comment: u64, reply: Option<String>, resolve: Option<bool>) -> Action {
    Action::Thread(ThreadChange {
        comment,
        reply,
        resolve,
    })
}

/// Ask before resolving a thread without a reply, which leaves the reviewer
/// no reason. Without exactly one selected PR the action itself fails, so
/// there is nothing to ask.
async fn confirm_unanswered(ctx: &Context, selector: &ItemSelector, comment: u64) -> Result<()> {
    let workspace = ui::select_workspace(
        ctx,
        selector.workspace.clone(),
        ui::Fallback::CurrentDirectory,
    )
    .await?;
    let selection = Selection {
        kind: Some(ItemKind::Pr),
        input: selector.item.clone(),
    };
    let selected: Selected = client::request(
        &ctx.paths,
        Method::SelectItems {
            workspace,
            selection,
        },
    )
    .await?;
    let [item] = selected.items.as_slice() else {
        return Ok(());
    };
    let forge = ForgeRepo::from_pull_url(&item.url)?;
    forge.ensure_resolves_threads()?;
    let (number, _) = forge.pull(&item.url)?;
    let thread = ctx
        .progress(
            "Reading thread",
            forge.thread(&ctx.forge_account()?, number, comment),
        )
        .await?;
    if thread.replies == 0 {
        ensure!(
            ui::confirm(
                ctx,
                &format!("Thread {} has no reply.", thread.first),
                "--body-file with the reason, or --yes"
            )?,
            "thread resolution canceled"
        );
    }
    Ok(())
}

fn read_body(body: BodyArgs) -> Result<Option<String>> {
    match body.body_file {
        Some(path) => std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))
            .map(Some),
        None => Ok(body.body),
    }
}
