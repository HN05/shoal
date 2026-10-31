//! GitHub items as its REST API reports them, for item views.
use anyhow::{Context, Result};
use serde_json::Value;

use super::threads::{GitHubThread, threads};
use crate::{
    ForgeRepo,
    account::Account,
    backends::github_rest,
    items::{action::MergeMethod, link::ItemKind},
    review::view::{AutoMerge, Details, Review, login, review_comment, review_state, text_field},
    transport::api::Request,
};

pub(crate) async fn details(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
    comments: bool,
) -> Result<Details> {
    let read = Request::get(repo.item_endpoint(kind, number));
    let item = repo.send(account, &read);
    let discussion = async {
        if !comments {
            return Ok(None);
        }
        let endpoint = repo.repo_endpoint(&format!("issues/{number}/comments"));
        repo.pages(account, &endpoint).await.map(Some)
    };
    let (item, discussion) = tokio::try_join!(item, discussion)?;
    let mut details = github_rest::details(&item, kind, number, repo.owner())?;
    if let Some(pr) = details.pr.as_mut() {
        pr.auto_merge = auto_merge(&item["auto_merge"])?;
    }
    details.comments = discussion.as_deref().map(github_rest::comments);
    Ok(details)
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
        let endpoint = repo.repo_endpoint(&format!("pulls/{number}/{resource}"));
        async move { repo.pages(account, &endpoint).await }
    };
    let (reviews, comments, threads) = tokio::try_join!(
        pages("reviews"),
        pages("comments"),
        threads(repo, account, number)
    )?;
    parse_reviews(&reviews, &comments, &threads).map(Some)
}

/// The automatic merge GitHub reports for a PR, if one is set.
fn auto_merge(request: &Value) -> Result<Option<AutoMerge>> {
    if request.is_null() {
        return Ok(None);
    }
    let method = match request["merge_method"].as_str() {
        Some("merge") => MergeMethod::Merge,
        Some("rebase") => MergeMethod::Rebase,
        Some("squash") => MergeMethod::Squash,
        method => anyhow::bail!("unknown auto-merge method {method:?}"),
    };
    Ok(Some(AutoMerge {
        method: Some(method),
    }))
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
