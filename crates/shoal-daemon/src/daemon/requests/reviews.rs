//! The daemon's answers to [`ReviewMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{self, Body, ReviewMethod, ReviewPiece};
use shoal_protocol::reviews::Review;
use shoal_protocol::space_names::SpaceSelector;
use shoal_reviews::written::Writer;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ReviewMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    let (reviews, daemon) = (&manager.reviews, &**manager);
    let viewer = Writer::of(caller.map(|caller| &caller.space_id));
    let shown = match method {
        ReviewMethod::AwaitReview { workspace, id } => {
            let (manager, reviews) = (manager.clone(), manager.reviews.clone());
            return Ok(Answer::wait(async move {
                let id = match id {
                    Some(id) => id,
                    None => reviews.latest(&*manager, &named(workspace)?).await?.id,
                };
                Ok(Body::ReviewId(reviews.wait(&*manager, id).await?.id))
            }));
        }
        ReviewMethod::CheckPrGate { workspace } => {
            manager
                .links
                .check_pr_review_gate(&**manager, &workspace, caller)
                .await?;
            return Ok(Answer::Body(Body::Ok));
        }
        ReviewMethod::RunReview {
            workspace,
            target,
            agent,
            model,
        } => pieces(
            reviews
                .queue(daemon, &workspace, target, agent, model)
                .await,
        ),
        ReviewMethod::StartReview { workspace } => {
            pieces(reviews.start_written(daemon, &workspace, viewer).await)
        }
        ReviewMethod::SubmitReview {
            workspace,
            approve,
            summary,
        } => {
            let submitted = reviews.submit(daemon, workspace.as_ref(), viewer, approve, summary);
            pieces(submitted.await)
        }
        ReviewMethod::PendingReviews => pieces(reviews.pending_reviews(daemon, viewer).await),
        ReviewMethod::CancelReview {
            workspace,
            id,
            here,
        } => {
            let by = caller.map(|caller| caller.space_id.clone());
            pieces(match (workspace, id, here) {
                (Some(workspace), None, true) => reviews.cancel_here(daemon, &workspace, by).await,
                (workspace, id, _) => reviews.cancel(daemon, workspace.as_ref(), id, by).await,
            })
        }
        ReviewMethod::ListReviews {
            workspace,
            repository,
        } => pieces(
            async {
                let ids = (reviews.workspaces(daemon, workspace.as_ref(), repository)).await?;
                let mut seen = Vec::new();
                for review in reviews.list(daemon, ids).await? {
                    seen.push(seen_by(manager, caller, review).await?);
                }
                Ok(seen)
            }
            .await,
        ),
        ReviewMethod::ShowReview { workspace, id } => pieces(
            async {
                let review = match id {
                    Some(id) => reviews.review(daemon, id).await?,
                    None => reviews.latest(daemon, &named(workspace)?).await?,
                };
                seen_by(manager, caller, review).await
            }
            .await,
        ),
    };
    Ok(Answer::pages(shown, Body::ReviewPieces))
}

/// Reviews as pieces of their JSON, each fitting a frame: one review's
/// threads may outgrow a frame of their own.
fn pieces<T: serde::Serialize>(shown: Result<T>) -> Result<Vec<ReviewPiece>> {
    let json = serde_json::to_string(&shown?)?;
    Ok(protocol::text_pieces(&json)
        .into_iter()
        .map(ReviewPiece)
        .collect())
}

/// `review` as `caller`, or the user without one, sees it: with only the
/// pending comments `caller` reads as threads.
pub(in crate::daemon) async fn seen_by(
    manager: &Manager,
    caller: Option<&Caller>,
    mut review: Review,
) -> Result<Review> {
    let mut read = Vec::new();
    for thread in std::mem::take(&mut review.threads) {
        if manager
            .coordination
            .reads_held(manager, caller, &thread)
            .await?
        {
            read.push(thread);
        }
    }
    review.threads = read;
    Ok(review)
}

/// The workspace a request without a review ID names.
fn named(workspace: Option<SpaceSelector>) -> Result<SpaceSelector> {
    workspace.ok_or_else(|| anyhow::anyhow!("name a review or a workspace"))
}

impl Checked for ReviewMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            // Its reviews are the caller's own.
            Self::PendingReviews => None,
            Self::RunReview { .. }
            | Self::StartReview { .. }
            | Self::SubmitReview { .. }
            | Self::CancelReview { .. }
            | Self::ListReviews { .. }
            | Self::ShowReview { .. }
            | Self::AwaitReview { .. }
            | Self::CheckPrGate { .. } => Some(TargetCheck::Space),
        }
    }
}

impl Scoped for ReviewMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            // A review named by ID is checked as its own workspace's.
            ReviewMethod::CancelReview { workspace, id, .. }
            | ReviewMethod::ShowReview { workspace, id }
            | ReviewMethod::AwaitReview { workspace, id } => {
                if let Some(id) = id {
                    *workspace = Some(SpaceSelector::id(&manager.reviews.workspace_of(*id).await?));
                }
                Some(caller.own_by_default(workspace))
            }
            ReviewMethod::RunReview { workspace, .. }
            | ReviewMethod::StartReview { workspace }
            | ReviewMethod::CheckPrGate { workspace } => Some(workspace),
            ReviewMethod::PendingReviews => None,
            // Without a workspace, it is the caller's own review.
            ReviewMethod::SubmitReview { workspace, .. } => workspace.as_mut(),
            ReviewMethod::ListReviews {
                workspace,
                repository,
            } => {
                if repository.is_some() && workspace.is_none() {
                    anyhow::bail!(
                        "workspace processes list only the reviews of their workspace tree; name a workspace"
                    );
                }
                Some(caller.own_by_default(workspace))
            }
        })
    }
}
