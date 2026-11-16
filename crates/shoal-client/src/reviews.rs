//! Local reviews: a one-shot, read-only reviewer's findings on a workspace's
//! committed HEAD or a PR, which the daemon stores and the workspace's agent
//! answers with typed outcomes.
use shoal_protocol::reviews::{FindingId, Outcome, Review, ReviewId, ReviewTarget};

use anyhow::{Context, Result};

use crate::connection::{Daemon, await_reply, pages, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    agent::{Agent, ModelChoice},
    protocol::{ReviewMethod, ReviewPiece},
    repository::Selector,
};

/// Queue a review of `target` in `workspace` by `agent` with `model`.
pub async fn run(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    target: ReviewTarget,
    agent: Agent,
    model: ModelChoice,
) -> Result<Review> {
    let method = ReviewMethod::RunReview {
        workspace,
        target,
        agent: Some(agent),
        model,
    };
    reassembled(daemon, method).await
}

/// Cancel the review `id`, or `workspace`'s queued or running one.
pub async fn cancel(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
    id: Option<ReviewId>,
) -> Result<Review> {
    reassembled(daemon, ReviewMethod::CancelReview { workspace, id }).await
}

/// The reviews of `workspace` or `repository`, or every review, newest first.
pub async fn list(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
    repository: Option<Selector>,
) -> Result<Vec<Review>> {
    let method = ReviewMethod::ListReviews {
        workspace,
        repository,
    };
    reassembled(daemon, method).await
}

/// The review `id`, or `workspace`'s latest.
pub async fn show(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
    id: Option<ReviewId>,
) -> Result<Review> {
    reassembled(daemon, ReviewMethod::ShowReview { workspace, id }).await
}

/// The ID of the review `id`, or of `workspace`'s latest, once it has
/// ended, waiting across daemon restarts.
pub async fn await_ended(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
    id: Option<ReviewId>,
) -> Result<ReviewId> {
    let method = || ReviewMethod::AwaitReview {
        workspace: workspace.clone(),
        id,
    };
    ReviewId::try_from(await_reply(daemon, method, false).await?)
}

/// Reply `body` to `finding`.
pub async fn reply(daemon: &impl Daemon, finding: FindingId, body: String) -> Result<Review> {
    let method = ReviewMethod::ReplyToFinding {
        workspace: None,
        finding,
        body,
    };
    reassembled(daemon, method).await
}

/// Resolve `finding` with `outcome`, or reopen it without one.
pub async fn resolve(
    daemon: &impl Daemon,
    finding: FindingId,
    outcome: Option<Outcome>,
) -> Result<Review> {
    let method = ReviewMethod::ResolveFinding {
        workspace: None,
        finding,
        outcome,
    };
    reassembled(daemon, method).await
}

/// Refuse a PR for `workspace` that `[review] require_for_pr` holds back
/// until a finished review covers HEAD.
pub async fn check_pr_gate(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<()> {
    request(daemon, ReviewMethod::CheckPrGate { workspace }).await
}

/// What a review request returns, a review or a listing of them,
/// reassembled from the pieces of JSON the daemon sends it in.
async fn reassembled<T: serde::de::DeserializeOwned>(
    daemon: &impl Daemon,
    method: ReviewMethod,
) -> Result<T> {
    let pieces = pages::<ReviewPiece>(daemon, method).await?;
    let json: String = pieces.into_iter().map(|piece| piece.0).collect();
    serde_json::from_str(&json).context("the daemon sent an invalid review")
}
