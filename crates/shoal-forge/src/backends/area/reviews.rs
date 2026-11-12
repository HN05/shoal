//! What each forge does its own way with reviewers, review threads, PR diffs
//! and submitted reviews.
use anyhow::Result;
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{ForgeKind, Items},
    items::item::Current,
    numbers::{CommentId, PullNumber},
    review::{
        diff::Diff,
        reviewers::Reviewer,
        submit::{PlacedReview, Submission},
        thread::Thread,
    },
    transport::api::Request,
};

/// How the forge asks for reviews, answers review threads and submits
/// reviews of a PR's diff.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Reviews: Items {
    /// The users and teams the repository's PRs can ask for a review.
    async fn reviewers(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>>;

    /// The review thread of `comment` on PR `number`, when it is in one.
    async fn thread(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        comment: CommentId,
    ) -> Result<Option<Thread>>;

    /// The request that replies in `thread` on PR `number`.
    fn reply(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        body: &str,
    ) -> Result<Request>;

    /// The request that resolves or unresolves `thread` on PR `number`.
    fn resolution(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request>;

    /// The diff of the PR `current`, at its head commit where the forge
    /// reads a diff there.
    async fn pr_diff(&self, repo: &ForgeRepo, account: &Account, current: &Current)
    -> Result<Diff>;

    /// Refuse a review of PR `number` as `account` that submitting would
    /// change more than the review on the forge.
    async fn check_review(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: PullNumber,
    ) -> Result<()> {
        Ok(())
    }

    /// The requests that submit `review` of PR `number`.
    fn review(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        review: &PlacedReview,
    ) -> Result<Submission>;

    /// The request that deletes the review draft a draft request's
    /// response describes, where the forge keeps drafts.
    fn discard_draft(
        &self,
        _repo: &ForgeRepo,
        _number: PullNumber,
        _draft: &Value,
    ) -> Result<Request> {
        anyhow::bail!("{} keeps no review drafts", self.name())
    }

    /// The activity keys of what `review` of PR `number` posted, from the
    /// `responses` to its submission's requests: by default each response
    /// describes a posted comment.
    async fn review_keys(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: PullNumber,
        _review: &PlacedReview<'_>,
        responses: &[Value],
    ) -> Result<Vec<String>> {
        let key = self.comment_key();
        Ok(responses
            .iter()
            .filter_map(|response| crate::updates::posted_key(key, response))
            .collect())
    }
}
