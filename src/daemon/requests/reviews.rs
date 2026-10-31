//! The daemon's answers to [`ReviewMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{self, Body, ReviewMethod, ReviewPiece};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ReviewMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    let shown = match method {
        ReviewMethod::AwaitReview { workspace, id } => {
            let manager = manager.clone();
            return Ok(Answer::wait(async move {
                let id = match id {
                    Some(id) => id,
                    None => manager.latest_review(&named(workspace)?).await?.id,
                };
                Ok(Body::ReviewId(manager.await_review(id).await?.id))
            }));
        }
        ReviewMethod::CheckPrGate { workspace } => {
            if caller.is_some() {
                manager.check_pr_review_gate(&workspace).await?;
            }
            return Ok(Answer::Body(Body::Ok));
        }
        ReviewMethod::RunReview {
            workspace,
            target,
            agent,
        } => pieces(manager.queue_review(&workspace, target, agent).await),
        ReviewMethod::CancelReview { workspace, id } => {
            let by = caller.map(|caller| caller.workspace_id.clone());
            pieces(manager.cancel_review(workspace.as_deref(), id, by).await)
        }
        ReviewMethod::ListReviews {
            workspace,
            repository,
        } => pieces(
            async {
                let ids = manager
                    .review_workspaces(workspace.as_deref(), repository)
                    .await?;
                manager.reviews(ids).await
            }
            .await,
        ),
        ReviewMethod::ShowReview { workspace, id } => pieces(match id {
            Some(id) => manager.review(id).await,
            None => match named(workspace) {
                Ok(workspace) => manager.latest_review(&workspace).await,
                Err(error) => Err(error),
            },
        }),
        ReviewMethod::ReplyToFinding { finding, body, .. } => {
            pieces(manager.reply_to_finding(finding, body).await)
        }
        ReviewMethod::ResolveFinding {
            finding, outcome, ..
        } => pieces(manager.resolve_finding(finding, outcome).await),
    };
    Ok(Answer::pages(shown, Body::ReviewPieces))
}

/// Reviews as pieces of their JSON, each fitting a frame: one review's
/// findings and replies may outgrow a frame of their own.
fn pieces<T: serde::Serialize>(shown: Result<T>) -> Result<Vec<ReviewPiece>> {
    let json = serde_json::to_string(&shown?)?;
    Ok(protocol::text_pieces(&json)
        .into_iter()
        .map(ReviewPiece)
        .collect())
}

/// The workspace a request without a review ID names.
fn named(workspace: Option<String>) -> Result<String> {
    workspace.ok_or_else(|| anyhow::anyhow!("name a review or a workspace"))
}

impl Scoped for ReviewMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            // A review named by ID is checked as its own workspace's.
            ReviewMethod::CancelReview { workspace, id }
            | ReviewMethod::ShowReview { workspace, id }
            | ReviewMethod::AwaitReview { workspace, id } => {
                if let Some(id) = id {
                    *workspace = Some(manager.review_workspace(*id).await?);
                }
                Some(caller.own_by_default(workspace))
            }
            ReviewMethod::ReplyToFinding {
                workspace, finding, ..
            }
            | ReviewMethod::ResolveFinding {
                workspace, finding, ..
            } => Some(workspace.insert(manager.review_workspace(finding.review).await?)),
            ReviewMethod::RunReview { workspace, .. } | ReviewMethod::CheckPrGate { workspace } => {
                Some(workspace)
            }
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
