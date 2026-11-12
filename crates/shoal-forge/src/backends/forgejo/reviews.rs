//! Forgejo and Gitea reviews: the users and teams they can ask for one, replies
//! in review threads, a PR's diff as the Git diff they serve, and a review with
//! its line comments in one request.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{Forgejo, threads};
use crate::{
    ForgeRepo,
    account::Account,
    backends::Reviews,
    capability::{self, Capability},
    items::item::Current,
    numbers::{CommentId, PullNumber},
    review::{
        diff::Diff,
        reviewers::Reviewer,
        submit::{PlacedReview, ReviewVerdict, Side, Submission},
        thread::{Target, Thread},
    },
    transport::api::{HttpMethod, Request},
    updates::activity_key,
};

mod reviewers;

impl Reviews for Forgejo {
    /// The users Forgejo offers as reviewers, and the teams of an
    /// organization's repository.
    async fn reviewers(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>> {
        let (users, teams) = tokio::try_join!(
            reviewers::users(repo, account),
            reviewers::teams(repo, account),
        )?;
        Ok(users.into_iter().chain(teams).collect())
    }

    async fn thread(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        comment: CommentId,
    ) -> Result<Option<Thread>> {
        let reviews = threads::all_review_comments(repo, account, number).await?;
        Ok(threads::thread(&reviews, comment))
    }

    /// A reply is a comment of the thread's review on the first comment's
    /// lines.
    fn reply(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        body: &str,
    ) -> Result<Request> {
        let Target::Forgejo {
            review,
            path,
            line,
            extra_lines,
        } = &thread.target
        else {
            bail!("not a Forgejo review thread");
        };
        let side = if *line < 0 {
            "old_position"
        } else {
            "new_position"
        };
        Ok(Request::new(
            HttpMethod::Post,
            repo.repo_endpoint(&format!("pulls/{number}/reviews/{review}/comments")),
            Some(json!({ "body": body, "path": path, side: line.abs(),
                "extra_lines_count": extra_lines })),
        ))
    }

    fn resolution(
        &self,
        _repo: &ForgeRepo,
        _number: PullNumber,
        _thread: &Thread,
        _resolved: bool,
    ) -> Result<Request> {
        Err(capability::unsupported(self, Capability::ResolveThreads))
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

    async fn review_keys(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: PullNumber,
        _review: &PlacedReview<'_>,
        responses: &[Value],
    ) -> Result<Vec<String>> {
        keys(responses.first().context("the review was not posted")?)
    }
}

/// The PR's diff as Forgejo serves it.
async fn diff(repo: &ForgeRepo, account: &Account, current: &Current) -> Result<Diff> {
    let endpoint = repo.repo_endpoint(&format!("pulls/{}.diff", current.item.number));
    Diff::parse(&repo.read_text(account, &Request::get(endpoint)).await?)
}

/// The request that posts `review` of PR `number` with its line comments,
/// of the head commit Shoal read. A comment's position is its line on its
/// side, and zero on the other.
fn submit(repo: &ForgeRepo, number: PullNumber, review: &PlacedReview) -> Submission {
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
fn keys(response: &Value) -> Result<Vec<String>> {
    let id = response.get("id").context("the review has no ID")?;
    Ok(vec![activity_key("reviews", id)])
}

#[cfg(test)]
mod tests;
