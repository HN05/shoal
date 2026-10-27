//! `shoal plan`: settle an issue's plan with an agent in a chat before anyone
//! implements it. The chat posts the plan to the issue's body, which is all
//! an implementing agent receives.
use anyhow::{Context as _, Result, ensure};

use super::{
    chat::{self, Topic},
    issues::{self, Issue},
};
use crate::{
    cli::{context::Context, ui},
    config,
    forge::items::{item::Item, link::ItemKind, list::MAX_LIMIT},
    model::Repository,
};

/// Which issue labelled as needing a plan is planned without naming one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pick {
    Picker,
    Oldest,
    Newest,
}

pub(super) async fn start(
    ctx: &Context,
    issue: Option<String>,
    repository: Option<String>,
    pick: Pick,
    mut options: chat::Options,
) -> Result<i32> {
    chat::ensure_unscoped(ctx)?;
    options.check_name(ctx).await?;
    let repo = issues::item_repository(
        ctx,
        ItemKind::Issue,
        issue.as_deref(),
        repository,
        "shoal plan <url> --repo <repository>",
    )
    .await?;
    let settings = chat::settings(ctx, &repo).await?;
    // Refuse a named agent before asking for the issue.
    options.agent = chat::named_agent(&settings, options.agent)?;
    let input = match issue {
        Some(issue) => issue,
        None => labelled(ctx, &repo, &settings.issue, pick).await?,
    };
    let issue = issues::load(ctx, &repo, &input).await?;
    let topic = topic(&repo, &issue, &settings.issue);
    chat::begin(ctx, repo, settings, topic, options).await
}

/// The number of the open issue labelled `[issue] plan_label` that `pick` selects.
async fn labelled(
    ctx: &Context,
    repo: &Repository,
    labels: &config::Issue,
    pick: Pick,
) -> Result<String> {
    let label = labels.plan_label.as_deref().context(
        "set [issue] plan_label to choose among the issues that need a plan, or name an issue",
    )?;
    let issues = issues::origin_forge(ctx, repo)
        .await?
        .open_issues_labelled(&ctx.forge_account()?, label)
        .await?;
    let kind = format!("issues labelled {label}");
    // The forge lists the newest first.
    let issue = match pick {
        Pick::Picker => return ui::pick_item(ctx, "Issue> ", &kind, issues),
        Pick::Newest => issues.first(),
        Pick::Oldest => oldest(&issues, &kind)?,
    };
    Ok(issue
        .with_context(|| format!("no open {kind}"))?
        .number
        .to_string())
}

/// The last of `issues`, listed newest first, unless the listing stopped at
/// its limit before reaching the oldest.
fn oldest<'a>(issues: &'a [Item], kind: &str) -> Result<Option<&'a Item>> {
    ensure!(
        issues.len() < MAX_LIMIT,
        "the forge lists only the newest {MAX_LIMIT} open {kind}; name the oldest issue instead"
    );
    Ok(issues.last())
}

/// A chat that plans `issue`, reopened in a new tab by the issue's URL.
fn topic(repo: &Repository, issue: &Issue, labels: &config::Issue) -> Topic {
    Topic {
        relaunch: vec![
            "plan".into(),
            issue.url.clone().into(),
            "--repo".into(),
            repo.id.clone().into(),
        ],
        name: format!("{}-plan-{}", crate::repository::name(repo), issue.number),
        asks_name: false,
        opening: Some(prompt(issue, labels)),
    }
}

/// The task a planning chat starts on.
fn prompt(issue: &Issue, labels: &config::Issue) -> String {
    format!(
        "Plan issue #{number}: {title}\n{url}\n\n{body}\n\n\
         Settle a plan for this issue with the user before anyone implements it. Check the \
         issue against the default branch, then talk through what is unclear or still open \
         with the user here, one point at a time, recommending an answer for each. Keep the \
         discussion in this conversation: do not post questions to the forge or ask them with \
         `shoal questions ask`.\n\n\
         Once the user agrees, rewrite the issue's body with your forge CLI so it states the \
         goal and a Plan section with the decisions, the approach and what done looks like, \
         since an implementing agent receives only the body.{relabel} Open follow-up issues for \
         work the plan splits off. Then offer to start an agent on it with \
         `shoal add --issue {number} --agent <name>`.",
        number = issue.number,
        title = issue.title,
        url = issue.url,
        body = issue.details,
        relabel = relabel(labels),
    )
}

/// The sentence that swaps the issue's labels once its plan is settled.
fn relabel(labels: &config::Issue) -> String {
    match (
        labels.plan_label.as_deref(),
        labels.planned_label.as_deref(),
    ) {
        (Some(plan), Some(planned)) => {
            format!(" Then replace its `{plan}` label with `{planned}`.")
        }
        (Some(plan), None) => format!(" Then remove its `{plan}` label."),
        (None, Some(planned)) => format!(" Then add the `{planned}` label."),
        (None, None) => String::new(),
    }
}

#[cfg(test)]
mod tests;
