//! GitHub reviews: a PR's diff as the patches of its changed files, and
//! a review with its line comments in one request.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    account::Account,
    items::item::{Current, text},
    review::{
        diff::{Diff, FileDiff},
        submit::{PlacedReview, ReviewVerdict, Side, Submission},
    },
    transport::api::{HttpMethod, Request},
    updates::{activity_key, id_text},
};

/// The PR's diff as GitHub lists its files. A file too large for GitHub
/// to show has no patch, so none of its lines takes a comment.
pub(super) async fn diff(repo: &ForgeRepo, account: &Account, current: &Current) -> Result<Diff> {
    let endpoint = repo.repo_endpoint(&format!("pulls/{}/files", current.item.number));
    let files = repo.pages(account, &endpoint).await?;
    files
        .iter()
        .map(changed_file)
        .collect::<Result<_>>()
        .map(Diff::new)
}

/// A file as GitHub lists a PR's changed files.
fn changed_file(file: &Value) -> Result<FileDiff> {
    let path = text(&file["filename"]).context("a changed file has no name")?;
    let old_path = text(&file["previous_filename"]).unwrap_or_else(|| path.clone());
    FileDiff::new(path, old_path, file["patch"].as_str().unwrap_or_default())
}

/// The request that posts `review` of PR `number` with its line comments,
/// of the head commit Shoal read.
pub(super) fn review(repo: &ForgeRepo, number: u64, review: &PlacedReview) -> Submission {
    let event = match review.verdict {
        ReviewVerdict::Comment => "COMMENT",
        ReviewVerdict::Approve => "APPROVE",
        ReviewVerdict::RequestChanges => "REQUEST_CHANGES",
    };
    let comments: Vec<_> = review
        .comments
        .iter()
        .map(|placed| {
            let side = match placed.comment.side {
                Side::New => "RIGHT",
                Side::Old => "LEFT",
            };
            json!({
                "path": placed.file.path,
                "line": placed.comment.line,
                "side": side,
                "body": placed.comment.body,
            })
        })
        .collect();
    let mut body = json!({ "event": event, "comments": comments });
    if !review.body.is_empty() {
        body["body"] = review.body.into();
    }
    if let Some(head) = review.head {
        body["commit_id"] = head.into();
    }
    let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
    Submission::posting(vec![Request::new(HttpMethod::Post, endpoint, Some(body))])
}

/// The activity keys of the review a review post's `response` describes
/// and of its line comments, which GitHub lists under the review.
pub(super) async fn keys(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    review: &PlacedReview<'_>,
    response: &Value,
) -> Result<Vec<String>> {
    let node = response
        .get("node_id")
        .context("the review has no node ID")?;
    let mut keys = vec![activity_key("reviews", node)];
    if !review.comments.is_empty() {
        let id = id_text(response.get("id").context("the review has no ID")?);
        let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews/{id}/comments"));
        let comments = repo.pages(account, &endpoint).await?;
        keys.extend(
            (comments.iter().filter_map(|comment| comment.get("id")))
                .map(|id| activity_key("inline", id)),
        );
    }
    Ok(keys)
}

#[cfg(test)]
mod tests;
