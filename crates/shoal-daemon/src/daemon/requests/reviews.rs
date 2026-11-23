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
use shoal_protocol::space_names::SpaceSelector;
use shoal_reviews::written::Writer;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ReviewMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    let (reviews, daemon) = (&manager.reviews, &**manager);
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
            let writer = Writer::of(caller.map(|caller| &caller.space_id));
            pieces(reviews.start_written(daemon, &workspace, writer).await)
        }
        ReviewMethod::PendingReviews => {
            let writer = Writer::of(caller.map(|caller| &caller.space_id));
            pieces(reviews.pending_reviews(daemon, writer).await)
        }
        ReviewMethod::CancelReview { workspace, id } => {
            let by = caller.map(|caller| caller.space_id.clone());
            pieces(reviews.cancel(daemon, workspace.as_ref(), id, by).await)
        }
        ReviewMethod::ListReviews {
            workspace,
            repository,
        } => pieces(
            async {
                let ids = (reviews.workspaces(daemon, workspace.as_ref(), repository)).await?;
                reviews.list(daemon, ids).await
            }
            .await,
        ),
        ReviewMethod::ShowReview { workspace, id } => pieces(match id {
            Some(id) => reviews.review(daemon, id).await,
            None => match named(workspace) {
                Ok(workspace) => reviews.latest(daemon, &workspace).await,
                Err(error) => Err(error),
            },
        }),
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
            ReviewMethod::CancelReview { workspace, id }
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
