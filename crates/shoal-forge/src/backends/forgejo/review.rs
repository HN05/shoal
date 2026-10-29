//! Forgejo and Gitea reviews: a PR's diff as the Git diff they serve, and
//! a review with its line comments in one request.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    account::Account,
    items::item::Current,
    review::{
        diff::Diff,
        submit::{PlacedReview, ReviewVerdict, Side, Submission},
    },
    transport::api::{HttpMethod, Request},
    updates::activity_key,
};

/// The PR's diff as Forgejo serves it.
pub(super) async fn diff(repo: &ForgeRepo, account: &Account, current: &Current) -> Result<Diff> {
    let endpoint = repo.repo_endpoint(&format!("pulls/{}.diff", current.item.number));
    Diff::parse(&repo.read_text(account, &Request::get(endpoint)).await?)
}

/// The request that posts `review` of PR `number` with its line comments,
/// of the head commit Shoal read. A comment's position is its line on its
/// side, and zero on the other.
pub(super) fn review(repo: &ForgeRepo, number: u64, review: &PlacedReview) -> Submission {
    let event = match review.verdict {
        ReviewVerdict::Comment => "COMMENT",
        ReviewVerdict::Approve => "APPROVED",
        ReviewVerdict::RequestChanges => "REQUEST_CHANGES",
    };
    let comments: Vec<_> = review
        .comments
        .iter()
        .map(|placed| {
            let line = placed.comment.line;
            let (new, old) = match placed.comment.side {
                Side::New => (line, 0),
                Side::Old => (0, line),
            };
            json!({
                "path": placed.file.path,
                "new_position": new,
                "old_position": old,
                "body": placed.comment.body,
            })
        })
        .collect();
    let mut body = json!({ "event": event, "body": review.body, "comments": comments });
    if let Some(head) = review.head {
        body["commit_id"] = head.into();
    }
    let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
    Submission::posting(vec![Request::new(HttpMethod::Post, endpoint, Some(body))])
}

/// The activity key of the review a review post's `response` describes,
/// which counts its line comments.
pub(super) fn keys(response: &Value) -> Result<Vec<String>> {
    let id = response.get("id").context("the review has no ID")?;
    Ok(vec![activity_key("reviews", id)])
}

#[cfg(test)]
mod tests;
