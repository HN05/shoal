//! A chat that starts on an issue, such as one that settles the issue's
//! plan in plan mode before anyone implements it. The plan goes to the
//! issue's body, which is all an implementing agent receives.
use anyhow::{Context as _, Result, ensure};

use super::Topic;
use crate::{
    cli::{
        client,
        commands::issues::{self, Issue},
        context::Context,
        ui,
    },
    config,
    forge::items::{link::ItemKind, list::Search, reference::ItemInput},
    model::{LinkedItem, Relation, Repository},
};

/// The registered repository of the issue `input` names, else the one
/// `repository` names or the current one.
pub(super) async fn repository(
    ctx: &Context,
    input: Option<&ItemInput>,
    repository: Option<String>,
) -> Result<Repository> {
    let retry = "shoal start --mode chat --issue <url> --repo <repository>";
    issues::item_repository(ctx, ItemKind::Issue, input, repository, retry).await
}

/// The issue `input` names in `repo`, else in `mode` plan a picked open
/// issue labelled `[issue] plan_label`.
pub(super) async fn chosen(
    ctx: &Context,
    repo: &Repository,
    labels: &config::Issue,
    mode: &crate::config::modes::ModeName,
    input: Option<ItemInput>,
) -> Result<Issue> {
    let input = match input {
        Some(input) => input,
        None => {
            ensure!(
                *mode == config::modes::ModeName::PLAN,
                "--issue needs a number or URL outside plan mode"
            );
            labelled(ctx, repo, labels).await?
        }
    };
    let context = issues::read_context(ctx, repo).await?;
    issues::load(ctx, context, input).await
}

/// The number of an open issue labelled `[issue] plan_label`, picked.
async fn labelled(ctx: &Context, repo: &Repository, labels: &config::Issue) -> Result<ItemInput> {
    let label = labels.plan_label.as_deref().context(
        "set [issue] plan_label to choose among the issues that need a plan, or name an issue",
    )?;
    let context = issues::read_context(ctx, repo).await?;
    let search = Search::labelled(label);
    let issues = client::list_items(&ctx.paths, context, ItemKind::Issue, search, None).await?;
    ui::require_argument(ctx, "issue", "issue")?;
    let number = ui::pick_item(ctx, "Issue> ", &format!("issues labelled {label}"), issues)?;
    Ok(ItemInput::Number(number))
}

/// `topic` started on `issue`: named after the issue's title, assigned the
/// issue, and reopened in a new tab by the issue's URL.
pub(super) fn on(topic: Topic, issue: &Issue) -> Topic {
    Topic {
        name: Some(issue.title.clone()),
        relaunch: topic.relaunch.on_issue(&issue.url),
        subject: Some(issue.subject()),
        links: vec![(
            Relation::Assigned,
            LinkedItem {
                url: issue.url.clone(),
                title: Some(issue.title.clone()),
            },
        )],
        ..topic
    }
}

#[cfg(test)]
mod tests;
