//! `shoal plan`: settle an issue's plan with an agent in a chat before anyone
//! implements it. The chat posts the plan to the issue's body, which is all
//! an implementing agent receives.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use super::{
    chat::{self, Topic},
    issues::{self, Issue},
};
use crate::{
    agent::Agent,
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
    agent: Option<Agent>,
    here: bool,
    args: Vec<OsString>,
) -> Result<i32> {
    chat::ensure_unscoped(ctx)?;
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
    let agent = chat::named_agent(&settings, agent)?;
    let input = match issue {
        Some(issue) => issue,
        None => labelled(ctx, &repo, &settings.issue, pick).await?,
    };
    let issue = issues::load(ctx, &repo, &input).await?;
    let topic = topic(&repo, &issue, &settings.issue);
    chat::begin(ctx, repo, topic, agent, here, args).await
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
        label: format!("{} plan", issue.tab_label(repo)),
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
         `shoal ask`.\n\n\
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
mod tests {
    use super::*;

    fn labels(plan: Option<&str>, planned: Option<&str>) -> config::Issue {
        config::Issue {
            plan_label: plan.map(str::to_owned),
            planned_label: planned.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn the_oldest_issue_is_refused_beyond_the_listing_limit() {
        let item = |number: u64| -> Item {
            serde_json::from_value(serde_json::json!({
                "kind": "issue", "number": number, "url": "url", "title": "title",
                "state": "open", "labels": [],
            }))
            .unwrap()
        };
        let mut issues: Vec<_> = (0..MAX_LIMIT as u64 - 1).rev().map(item).collect();
        assert_eq!(oldest(&issues, "issues").unwrap().unwrap().number, 0);
        issues.insert(0, item(MAX_LIMIT as u64));
        let error = oldest(&issues, "issues").unwrap_err().to_string();
        assert!(
            error.contains("only the newest 1000 open issues"),
            "{error}"
        );
        assert!(oldest(&[], "issues").unwrap().is_none());
    }

    #[test]
    fn the_prompt_carries_the_issue_and_swaps_only_configured_labels() {
        let issue = Issue {
            number: 7,
            title: "Decide {title}".into(),
            url: "https://forge.example/team/repo/issues/7".into(),
            details: "Which way?".into(),
        };
        let prompt = prompt(&issue, &labels(Some("needs-plan"), Some("planned")));
        assert!(
            prompt.starts_with(
                "Plan issue #7: Decide {title}\nhttps://forge.example/team/repo/issues/7\n\nWhich way?\n\n"
            ),
            "{prompt}"
        );
        assert!(
            prompt.contains(
                "only the body. Then replace its `needs-plan` label with `planned`. Open"
            ),
            "{prompt}"
        );
        assert!(prompt.ends_with("`shoal add --issue 7 --agent <name>`."));
        assert_eq!(
            relabel(&labels(Some("needs-plan"), None)),
            " Then remove its `needs-plan` label."
        );
        assert_eq!(
            relabel(&labels(None, Some("planned"))),
            " Then add the `planned` label."
        );
        assert_eq!(relabel(&labels(None, None)), "");
    }
}
