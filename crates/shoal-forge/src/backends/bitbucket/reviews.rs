//! Bitbucket Cloud reviews: a pull request's reviewers and comment threads, its
//! diff as the Git diff Bitbucket serves between two commits, and a review as a
//! comment for each line, then the body, then the verdict.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{Bitbucket, listed, roots};
use crate::{
    ForgeRepo,
    account::Account,
    backends::Reviews,
    items::{
        item::{Current, text},
        link::ItemKind,
    },
    numbers::{CommentId, PullNumber},
    review::{
        diff::Diff,
        reviewers::Reviewer,
        submit::{PlacedReview, ReviewVerdict, Side, Submission},
        thread::{Target, Thread},
    },
    transport::api::{HttpMethod, Request},
};

impl Reviews for Bitbucket {
    /// The members of the repository's workspace, by Atlassian account ID.
    async fn reviewers(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>> {
        let endpoint = format!("workspaces/{}/members", repo.owner());
        let members = repo.pages(account, &endpoint).await?;
        Ok(members
            .iter()
            .filter_map(|member| {
                let user = &member["user"];
                Some(Reviewer::user(
                    text(&user["account_id"])?,
                    user["display_name"].as_str(),
                ))
            })
            .collect())
    }

    async fn thread(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        comment: CommentId,
    ) -> Result<Option<Thread>> {
        let pr = repo.item_endpoint(ItemKind::Pr, number.get());
        let comments = repo.pages(account, &format!("{pr}/comments")).await?;
        Ok(find_thread(&comments, comment))
    }

    fn reply(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        body: &str,
    ) -> Result<Request> {
        let pr = repo.item_endpoint(ItemKind::Pr, number.get());
        Ok(Request::new(
            HttpMethod::Post,
            format!("{pr}/comments"),
            Some(json!({ "content": { "raw": body }, "parent": { "id": thread.first } })),
        ))
    }

    fn resolution(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request> {
        let pr = repo.item_endpoint(ItemKind::Pr, number.get());
        let method = if resolved {
            HttpMethod::Post
        } else {
            HttpMethod::Delete
        };
        Ok(Request::new(
            method,
            format!("{pr}/comments/{}/resolve", thread.first),
            None,
        ))
    }

    async fn pr_diff(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        current: &Current,
    ) -> Result<Diff> {
        diff(repo, account, current).await
    }

    fn review(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        review: &PlacedReview,
    ) -> Result<Submission> {
        Ok(submit(repo, number, review))
    }
}

/// The pull request's diff from where its head forks from the
/// destination, as Bitbucket shows it.
async fn diff(repo: &ForgeRepo, account: &Account, current: &Current) -> Result<Diff> {
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
fn submit(repo: &ForgeRepo, number: PullNumber, review: &PlacedReview) -> Submission {
    let pull = repo.item_endpoint(ItemKind::Pr, number.get());
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

/// The thread with `comment`, named by its first comment.
fn find_thread(comments: &[Value], comment: CommentId) -> Option<Thread> {
    let comments = listed(comments);
    let root = roots(&comments);
    comments.iter().find(|value| value["id"] == comment)?;
    let first = root(comment.get());
    let start = comments.iter().find(|value| value["id"] == first)?;
    let replies = comments
        .iter()
        .filter_map(|value| value["id"].as_u64())
        .filter(|id| *id != first && root(*id) == first)
        .count();
    Some(Thread {
        first: CommentId::new(first),
        replies,
        resolved: Some(start["resolution"].is_object()),
        target: Target::Bitbucket,
    })
}

#[cfg(test)]
mod tests;
