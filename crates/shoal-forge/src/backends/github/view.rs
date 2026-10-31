//! GitHub items as `gh` and its REST API report them, for item views.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::{
    Query, gh,
    threads::{GitHubThread, threads},
};
use crate::{
    ForgeRepo,
    account::Account,
    items::{action::MergeMethod, link::ItemKind},
    review::view::{
        AutoMerge, Comment, Details, PrDetails, Review, labels, login, required, review_comment,
        review_state, text_field,
    },
};

pub(crate) async fn details(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
    comments: bool,
) -> Result<Details> {
    let id = number.to_string();
    let repository = format!("{}/{}", repo.host, repo.path);
    let (command, extra) = match kind {
        ItemKind::Issue => ("issue", ""),
        ItemKind::Pr => (
            "pr",
            ",headRefName,baseRefName,isDraft,autoMergeRequest,reviewRequests",
        ),
    };
    let fields = format!(
        "number,title,body,state,author,labels,createdAt{extra}{}",
        if comments { ",comments" } else { "" }
    );
    let args = [
        command,
        "view",
        &id,
        "--repo",
        &repository,
        "--json",
        &fields,
    ];
    let output = gh(account, &args, Query::View).await?;
    parse_details(&output, kind, number)
}

/// Reviews with their inline comments; issues have none.
pub(crate) async fn reviews(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
) -> Result<Option<Vec<Review>>> {
    if kind == ItemKind::Issue {
        return Ok(None);
    }
    let pages = |resource: &'static str| {
        let endpoint = format!("repos/{}/pulls/{number}/{resource}", repo.path);
        async move {
            let args = [
                "api",
                "--hostname",
                &repo.host,
                &endpoint,
                "--paginate",
                "--slurp",
            ];
            let output = gh(account, &args, Query::View).await?;
            let pages: Vec<Vec<Value>> = serde_json::from_str(&output)
                .with_context(|| format!("invalid gh {resource} response"))?;
            Ok::<_, anyhow::Error>(pages.into_iter().flatten().collect::<Vec<_>>())
        }
    };
    let (reviews, comments, threads) = tokio::try_join!(
        pages("reviews"),
        pages("comments"),
        threads(repo, account, number)
    )?;
    parse_reviews(&reviews, &comments, &threads).map(Some)
}

fn parse_details(text: &str, kind: ItemKind, number: u64) -> Result<Details> {
    let item: Value = serde_json::from_str(text).context("invalid gh response")?;
    ensure!(item["number"] == number, "gh returned a different item");
    let pr = match kind {
        ItemKind::Issue => None,
        ItemKind::Pr => Some(PrDetails {
            auto_merge: auto_merge(&item["autoMergeRequest"])?,
            reviewers: review_requests(&item["reviewRequests"]),
            ..PrDetails::read(
                &item["headRefName"],
                &item["baseRefName"],
                item["isDraft"].as_bool().unwrap_or_default(),
            )?
        }),
    };
    let comments = match item.get("comments") {
        Some(comments) => Some(
            comments
                .as_array()
                .context("gh comments are not a list")?
                .iter()
                .map(|comment| Comment {
                    id: comment_id(&comment["url"]),
                    author: login(&comment["author"]),
                    created_at: text_field(&comment["createdAt"]),
                    body: text_field(&comment["body"]),
                    reactions: Vec::new(),
                })
                .collect(),
        ),
        None => None,
    };
    Ok(Details {
        number,
        title: required(&item["title"], "title")?,
        state: required(&item["state"], "state")?.to_ascii_lowercase(),
        author: login(&item["author"]),
        created_at: text_field(&item["createdAt"]),
        labels: labels(&item["labels"]),
        body: text_field(&item["body"]),
        pr,
        comments,
        reviews: None,
        reactions: Vec::new(),
    })
}

fn auto_merge(request: &Value) -> Result<Option<AutoMerge>> {
    if request.is_null() {
        return Ok(None);
    }
    let method = match request["mergeMethod"].as_str() {
        Some("MERGE") => MergeMethod::Merge,
        Some("REBASE") => MergeMethod::Rebase,
        Some("SQUASH") => MergeMethod::Squash,
        method => anyhow::bail!("unknown auto-merge method {method:?}"),
    };
    Ok(Some(AutoMerge {
        method: Some(method),
    }))
}

/// The users and teams asked for a review, which `gh` names by login and
/// by `owner/team` slug.
fn review_requests(requests: &Value) -> Vec<String> {
    requests
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|request| request["slug"].as_str().or(request["login"].as_str()))
        .map(str::to_owned)
        .collect()
}

/// The REST ID of the comment at `url`, which `gh` reports only there.
fn comment_id(url: &Value) -> u64 {
    url.as_str()
        .and_then(|url| url.rsplit_once("#issuecomment-"))
        .and_then(|(_, id)| id.parse().ok())
        .unwrap_or_default()
}

/// REST reviews and inline comments; each comment belongs to the review that
/// posted it, including replies, and takes its thread's resolution.
fn parse_reviews(
    reviews: &[Value],
    comments: &[Value],
    threads: &[GitHubThread],
) -> Result<Vec<Review>> {
    let mut result = Vec::new();
    for review in reviews {
        let Some(state) = review_state(review["state"].as_str())? else {
            continue;
        };
        let id = review["id"].as_u64().context("GitHub review has no ID")?;
        result.push(Review {
            author: login(&review["user"]),
            state,
            submitted_at: text_field(&review["submitted_at"]),
            body: text_field(&review["body"]),
            comments: comments
                .iter()
                .filter(|comment| comment["pull_request_review_id"].as_u64() == Some(id))
                .map(|comment| {
                    let id = comment["id"].as_u64().unwrap_or_default();
                    let thread = threads.iter().find(|thread| thread.comments.contains(&id));
                    review_comment(
                        comment,
                        &comment["line"],
                        &comment["original_line"],
                        comment["in_reply_to_id"].as_u64().unwrap_or(id),
                        thread.map(|thread| thread.resolved),
                    )
                })
                .collect(),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
