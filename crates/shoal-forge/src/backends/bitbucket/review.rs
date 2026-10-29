//! Bitbucket Cloud reviews: a pull request's diff as the Git diff
//! Bitbucket serves between two commits, and a review as a comment for
//! each line, then the body, then the verdict.
use anyhow::{Context, Result};
use serde_json::json;

use crate::{
    ForgeRepo,
    account::Account,
    items::{item::Current, link::ItemKind},
    review::{
        diff::Diff,
        submit::{PlacedReview, ReviewVerdict, Side, Submission},
    },
    transport::api::{HttpMethod, Request},
};

/// The pull request's diff from where its head forks from the
/// destination, as Bitbucket shows it.
pub(super) async fn diff(repo: &ForgeRepo, account: &Account, current: &Current) -> Result<Diff> {
    let request = Request::get(diff_endpoint(repo, current)?);
    Diff::parse(&repo.read_text(account, &request).await?)
}

/// The endpoint of the diff from the merge base of the pull request's
/// head and destination commits to its head.
fn diff_endpoint(repo: &ForgeRepo, current: &Current) -> Result<String> {
    let head = current
        .head_sha
        .as_deref()
        .context("PR has no head commit")?;
    let base = (current.base_sha.as_deref()).context("PR has no destination commit")?;
    Ok(repo.repo_endpoint(&format!("diff/{head}..{base}?topic=true")))
}

/// The requests that post `review` of pull request `number` in order: an
/// inline comment on each line, the body as a comment, then approving or
/// requesting changes.
pub(super) fn review(repo: &ForgeRepo, number: u64, review: &PlacedReview) -> Submission {
    let pull = repo.item_endpoint(ItemKind::Pr, number);
    let comment = |body: serde_json::Value| {
        Request::new(HttpMethod::Post, format!("{pull}/comments"), Some(body))
    };
    let mut requests: Vec<_> = (review.comments.iter())
        .map(|placed| {
            let side = match placed.comment.side {
                Side::New => "to",
                Side::Old => "from",
            };
            comment(json!({
                "content": { "raw": placed.comment.body },
                "inline": { "path": placed.file.path, side: placed.comment.line },
            }))
        })
        .collect();
    if !review.body.is_empty() {
        requests.push(comment(json!({ "content": { "raw": review.body } })));
    }
    let verdict = match review.verdict {
        ReviewVerdict::Comment => None,
        ReviewVerdict::Approve => Some("approve"),
        ReviewVerdict::RequestChanges => Some("request-changes"),
    };
    requests.extend(
        verdict.map(|verdict| Request::new(HttpMethod::Post, format!("{pull}/{verdict}"), None)),
    );
    Submission::posting(requests)
}

#[cfg(test)]
mod tests;
