//! Local reviews: a one-shot, read-only reviewer's run over a workspace's
//! committed HEAD or a PR, whose findings are `review` threads.
use shoal_protocol::reviews::{Review, ReviewId, ReviewTarget};

use anyhow::{Context, Result};

use crate::connection::{Daemon, Sendable, await_reply, pages, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    agent::{Agent, ModelChoice},
    protocol::{ReviewMethod, ReviewPiece},
    repository::Selector,
};

/// Queue a review of `target` in `workspace` by `agent` with `model`.
pub fn run(
    workspace: SpaceSelector,
    target: ReviewTarget,
    agent: Agent,
    model: ModelChoice,
) -> PreparedReview<Review> {
    PreparedReview::new(ReviewMethod::RunReview {
        workspace,
        target,
        agent: Some(agent),
        model,
    })
}

/// Start the caller's own review of `workspace`, or return its pending one.
pub fn start(workspace: SpaceSelector) -> PreparedReview<Review> {
    PreparedReview::new(ReviewMethod::StartReview { workspace })
}

/// The caller's pending reviews, oldest first.
pub fn pending() -> PreparedReview<Vec<Review>> {
    PreparedReview::new(ReviewMethod::PendingReviews)
}

/// Submit the caller's pending review of `workspace`, else its only one,
/// with `summary`, approving the work when `approve`.
pub fn submit(
    workspace: Option<SpaceSelector>,
    approve: bool,
    summary: Option<String>,
) -> PreparedReview<Review> {
    PreparedReview::new(ReviewMethod::SubmitReview {
        workspace,
        approve,
        summary,
    })
}

/// Cancel the review `id`, or `workspace`'s queued or running one.
pub fn cancel(workspace: Option<SpaceSelector>, id: Option<ReviewId>) -> PreparedReview<Review> {
    PreparedReview::new(ReviewMethod::CancelReview { workspace, id })
}

/// The reviews of `workspace` or `repository`, or every review, newest first.
pub fn list(
    workspace: Option<SpaceSelector>,
    repository: Option<Selector>,
) -> PreparedReview<Vec<Review>> {
    PreparedReview::new(ReviewMethod::ListReviews {
        workspace,
        repository,
    })
}

/// The review `id`, or `workspace`'s latest.
pub fn show(workspace: Option<SpaceSelector>, id: Option<ReviewId>) -> PreparedReview<Review> {
    PreparedReview::new(ReviewMethod::ShowReview { workspace, id })
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

/// Refuse a PR for `workspace` that `[review] require_for_pr` holds back
/// until a finished review covers HEAD.
pub async fn check_pr_gate(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<()> {
    request(daemon, ReviewMethod::CheckPrGate { workspace }).await
}

/// What a review request returns, a review or a listing of them,
/// reassembled from the pieces of JSON the daemon sends it in.
/// A review request built before it is sent, whose answer comes in pieces.
pub struct PreparedReview<T> {
    method: ReviewMethod,
    answer: std::marker::PhantomData<fn() -> T>,
}

impl<T> PreparedReview<T> {
    fn new(method: ReviewMethod) -> Self {
        Self {
            method,
            answer: std::marker::PhantomData,
        }
    }
}

impl<T: serde::de::DeserializeOwned> PreparedReview<T> {
    /// Send it, with its answer.
    pub async fn send(self, daemon: &impl Daemon) -> Result<T> {
        reassembled(daemon, self.method).await
    }
}

impl<T> Sendable for PreparedReview<T> {
    fn into_operation(self) -> shoal_protocol::protocol::Operation {
        self.method.into()
    }
}

async fn reassembled<T: serde::de::DeserializeOwned>(
    daemon: &impl Daemon,
    method: ReviewMethod,
) -> Result<T> {
    let pieces = pages::<ReviewPiece>(daemon, method).await?;
    let json: String = pieces.into_iter().map(|piece| piece.0).collect();
    serde_json::from_str(&json).context("the daemon sent an invalid review")
}
