//! `shoal pr` and `shoal issue`: ask the daemon to list, open or
//! change items of the repository of the workspace or registered checkout and
//! report them as the forge now does.
use anyhow::Result;

use crate::{
    cli::{
        args::changes::{IssueCommand, ItemSelector, PrCommand},
        client,
        context::Context,
    },
    forge::items::{
        action::{Action, MergeMethod},
        create::NewIssue,
        item::{Item, ItemState},
        link::{ItemKind, LinkKind, Selection},
    },
    model::Selected,
    protocol::ItemMethod,
};

mod content;
mod context;
mod list;
mod open;
mod react;
mod revert;
mod review;
mod reviewers;
mod thread;

use self::{
    content::{comment, edit_fields, pr_edit, read_content},
    list::list,
    open::{open_issue, open_pr, pull_options},
    react::react,
    thread::{reply, resolve, thread},
};
pub(crate) use content::read_body;
pub(super) use context::{
    checkout_repository, current_context, select_item_context, select_repo_context,
};
pub(super) use open::push_and_open;

pub(super) async fn pr(ctx: &Context, command: PrCommand) -> Result<i32> {
    let (item, action, outcome) = match command {
        PrCommand::List(args) => return list(ctx, ItemKind::Pr, args).await,
        PrCommand::Open { workspace, pull } => {
            return open_pr(ctx, workspace, pull_options(pull)?).await;
        }
        PrCommand::Revert { item, revert } => return revert::revert(ctx, item, revert).await,
        PrCommand::Edit { item, edit } => (item, Action::edit(pr_edit(edit)?), "PR updated"),
        PrCommand::Reviewers { item } => return reviewers::reviewers(ctx, &item).await,
        PrCommand::Comment {
            item,
            body,
            files,
            edit,
        } => {
            let (action, outcome) = comment(body, files, edit, "Commented on PR")?;
            (item, action, outcome)
        }
        PrCommand::Review { item, review } => {
            let (action, outcome) = review::review(review)?;
            (item, action, outcome)
        }
        PrCommand::React { reaction, item } => {
            let (action, outcome) = react(reaction);
            (item, action, outcome)
        }
        PrCommand::Reply {
            comment,
            item,
            body,
        } => (item, reply(comment, body)?, "Replied in thread"),
        PrCommand::Resolve {
            comment,
            item,
            body,
            confirmation,
        } => {
            let action = resolve(ctx, &item, comment, body, confirmation.yes).await?;
            (item, action, "Thread resolved")
        }
        PrCommand::Unresolve { comment, item } => (
            item,
            thread(comment, None, Some(false)),
            "Thread unresolved",
        ),
        PrCommand::Close { item } => (item, Action::Close, "PR closed"),
        PrCommand::Reopen { item } => (item, Action::Reopen, "PR reopened"),
        PrCommand::Update { item, method } => {
            (item, Action::Update { method }, "PR branch updated")
        }
        PrCommand::Merge {
            item,
            method,
            delete_branch,
            auto,
            cancel_auto: _,
        } => {
            let (action, outcome) = merge(method, delete_branch, auto);
            (item, action, outcome)
        }
        PrCommand::DeleteBranch { item } => (item, Action::DeleteBranch, "PR branch deleted"),
    };
    act(ctx, item, ItemKind::Pr, action, outcome).await
}

pub(super) async fn issue(ctx: &Context, command: IssueCommand) -> Result<i32> {
    let (item, action, outcome) = match command {
        IssueCommand::List(args) => return list(ctx, ItemKind::Issue, args).await,
        IssueCommand::Open {
            workspace,
            repo,
            title,
            body,
            files,
            labels,
            assignees,
            milestone,
            link,
        } => {
            let (body, attachments) = read_content(body, files)?;
            let issue = NewIssue {
                title,
                body: body.unwrap_or_default(),
                labels,
                assignees,
                milestone,
                attachments,
            };
            return open_issue(ctx, workspace, issue, repo, link).await;
        }
        IssueCommand::Edit { item, edit } => {
            (item, Action::edit(edit_fields(edit)?), "Issue updated")
        }
        IssueCommand::Comment {
            item,
            body,
            files,
            edit,
        } => {
            let (action, outcome) = comment(body, files, edit, "Commented on issue")?;
            (item, action, outcome)
        }
        IssueCommand::React { reaction, item } => {
            let (action, outcome) = react(reaction);
            (item, action, outcome)
        }
        IssueCommand::Close { item } => (item, Action::Close, "Issue closed"),
        IssueCommand::Reopen { item } => (item, Action::Reopen, "Issue reopened"),
    };
    act(ctx, item, ItemKind::Issue, action, outcome).await
}

/// The merge the options ask for: without a method, which only
/// `--cancel-auto` leaves out, withdrawing an automatic merge.
fn merge(method: Option<MergeMethod>, delete_branch: bool, auto: bool) -> (Action, &'static str) {
    match method {
        None => (Action::CancelAutoMerge, "Auto-merge canceled"),
        Some(method) if auto => (
            Action::AutoMerge {
                method,
                delete_branch,
            },
            "Auto-merge enabled",
        ),
        Some(method) => (
            Action::Merge {
                method,
                delete_branch,
            },
            "PR merged",
        ),
    }
}

async fn act(
    ctx: &Context,
    selector: ItemSelector,
    kind: ItemKind,
    action: Action,
    outcome: &str,
) -> Result<i32> {
    action.validate(kind)?;
    // The forge merges now a PR whose checks already pass.
    let auto_merge = matches!(action, Action::AutoMerge { .. });
    let item: Item = client::request(
        &ctx.paths,
        ItemMethod::ItemAction {
            context: select_item_context(
                ctx,
                selector.workspace,
                selector.repo.name,
                selector.item.as_deref(),
            )
            .await?,
            kind,
            item: selector.item,
            action,
        },
    )
    .await?;
    let outcome = if auto_merge && item.state == ItemState::Merged {
        "PR merged"
    } else {
        outcome
    };
    ctx.emit(&format!("{outcome}: {}", item.url), &item)?;
    Ok(0)
}

/// The PR `selector` names, or the workspace's linked ones.
async fn select_prs(ctx: &Context, selector: &ItemSelector) -> Result<Selected> {
    let context = select_item_context(
        ctx,
        selector.workspace.clone(),
        selector.repo.name.clone(),
        selector.item.as_deref(),
    )
    .await?;
    let selection = Selection {
        kind: Some(LinkKind::Pr),
        input: selector.item.clone(),
    };
    super::links::select_items(ctx, &context, selection, false).await
}
