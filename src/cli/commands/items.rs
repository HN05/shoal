//! `shoal pr` and `shoal issue`: ask the daemon to change an item of the
//! workspace's repository and report it as the forge now does.
use anyhow::{Context as _, Result};

use crate::{
    cli::{
        BodyArgs, EditArgs, IssueCommand, ItemSelector, PrCommand, client, context::Context, ui,
    },
    forge::{
        action::{Action, Edit},
        item::Item,
        link::ItemKind,
    },
    protocol::Method,
};

pub(super) async fn pr(ctx: &Context, command: PrCommand) -> Result<i32> {
    let (item, action, outcome) = match command {
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
