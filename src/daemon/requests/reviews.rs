//! The daemon's answers to [`ReviewMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, ReviewMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ReviewMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    let review = |review| Body::Review(Box::new(review));
    Ok(Answer::Body(match method {
        ReviewMethod::AwaitReview { workspace, id } => {
            let manager = manager.clone();
            return Ok(Answer::wait(async move {
                let id = match id {
                    Some(id) => id,
                    None => manager.latest_review(&named(workspace)?).await?.id,
                };
                Ok(Body::Review(Box::new(manager.await_review(id).await?)))
            }));
        }
        ReviewMethod::RunReview {
            workspace,
            target,
            agent,
        } => review(manager.queue_review(&workspace, target, agent).await?),
        ReviewMethod::CancelReview { workspace, id } => {
            let by = caller.map(|caller| caller.workspace_id.clone());
            review(manager.cancel_review(workspace.as_deref(), id, by).await?)
        }
        ReviewMethod::ListReviews {
            workspace,
            repository,
        } => {
            let ids = manager
                .review_workspaces(workspace.as_deref(), repository)
                .await?;
            Body::Reviews(manager.reviews(ids).await?)
        }
        ReviewMethod::ShowReview { workspace, id } => review(match id {
            Some(id) => manager.review(id).await?,
            None => manager.latest_review(&named(workspace)?).await?,
        }),
        ReviewMethod::CheckPrGate { workspace } => {
            if caller.is_some() {
                manager.check_pr_review_gate(&workspace).await?;
            }
            Body::Ok
        }
        ReviewMethod::ReplyToFinding { finding, body, .. } => {
            review(manager.reply_to_finding(finding, body).await?)
        }
        ReviewMethod::ResolveFinding {
            finding, outcome, ..
        } => review(manager.resolve_finding(finding, outcome).await?),
    }))
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
