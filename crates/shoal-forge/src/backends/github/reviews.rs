//! GitHub reviews: the users and teams it can ask for one, replies to and
//! resolutions of review threads, a PR's diff as the patches of its changed
//! files, and a review with its line comments in one request.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{GitHub, threads};
use crate::{
    ForgeRepo,
    account::Account,
    backends::Reviews,
    items::item::{Current, text},
    numbers::{CommentId, PullNumber},
    review::{
        diff::{Diff, FileDiff},
        reviewers::Reviewer,
        submit::{PlacedReview, ReviewVerdict, Side, Submission},
        thread::{Target, Thread},
    },
    transport::api::{HttpMethod, Request},
    updates::{activity_key, id_text},
};

mod reviewers;

impl Reviews for GitHub {
    /// The users GitHub can assign the repository's items to, and the
    /// teams of an organization's repository.
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
        Ok(threads::thread(
            &threads::threads(repo, account, number).await?,
            comment,
        ))
    }

    fn reply(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        body: &str,
    ) -> Result<Request> {
        let endpoint =
            repo.repo_endpoint(&format!("pulls/{number}/comments/{}/replies", thread.first));
        Ok(Request::new(
            HttpMethod::Post,
            endpoint,
            Some(json!({ "body": body })),
        ))
    }

    fn resolution(
        &self,
        _repo: &ForgeRepo,
        _number: PullNumber,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request> {
        let Target::GitHub { node } = &thread.target else {
            bail!("not a GitHub review thread");
        };
        let mutation = if resolved {
            "resolveReviewThread"
        } else {
            "unresolveReviewThread"
        };
        Ok(Request::new(
            HttpMethod::Post,
            "graphql".into(),
            Some(json!({
                "query": format!(
                    "mutation($id: ID!) {{ {mutation}(input: {{threadId: $id}}) {{ thread {{ isResolved }} }} }}"
                ),
                "variables": { "id": node },
            })),
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

    async fn review_keys(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        review: &PlacedReview<'_>,
        responses: &[Value],
    ) -> Result<Vec<String>> {
        let response = responses.first().context("the review was not posted")?;
        keys(repo, account, number, review, response).await
    }
}

/// The PR's diff as GitHub lists its files. A file too large for GitHub
/// to show has no patch, so none of its lines takes a comment.
async fn diff(repo: &ForgeRepo, account: &Account, current: &Current) -> Result<Diff> {
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
fn submit(repo: &ForgeRepo, number: PullNumber, review: &PlacedReview) -> Submission {
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
async fn keys(
    repo: &ForgeRepo,
    account: &Account,
    number: PullNumber,
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
