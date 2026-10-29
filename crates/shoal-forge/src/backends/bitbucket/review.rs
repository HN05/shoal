//! Bitbucket Cloud reviews: a pull request's diff as the Git diff
//! Bitbucket serves between two commits.
use anyhow::{Context, Result};

use crate::{
    ForgeRepo,
    account::Account,
    items::{item::Current, link::ItemKind},
    review::{
        diff::Diff,
        submit::{PlacedReview, Submission},
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

/// The request that posts `review` of pull request `number`, its body as
/// a comment.
pub(super) fn review(repo: &ForgeRepo, number: u64, review: &PlacedReview) -> Submission {
    let endpoint = format!("{}/comments", repo.item_endpoint(ItemKind::Pr, number));
    let body = serde_json::json!({ "content": { "raw": review.body } });
    Submission::posting(vec![Request::new(HttpMethod::Post, endpoint, Some(body))])
}

#[cfg(test)]
mod tests;
