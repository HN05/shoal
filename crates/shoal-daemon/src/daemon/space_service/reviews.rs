//! The reviews the spaces' gates require, and those the spaces queue, carry
//! and stop.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::approvals::ApprovalRequest;
use shoal_protocol::model::Workspace;
use shoal_protocol::reviews::CarriedReview;
use shoal_refs::CommitId;
use shoal_spaces::daemon::ReviewGate;

use crate::daemon::{manager::Manager, scope::Caller};

/// The reviews' gate the spaces' `gate` is.
pub(super) fn review_gate(gate: ReviewGate) -> shoal_reviews::required::Gate {
    match gate {
        ReviewGate::Ready => shoal_reviews::required::Gate::Ready,
        ReviewGate::Merge => shoal_reviews::required::Gate::Merge,
    }
}

impl shoal_spaces::daemon::Reviews for Manager {
    fn check_review<'a>(
        &'a self,
        workspace: &'a Workspace,
        gate: ReviewGate,
        head: &'a CommitId,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.reviews
                .check(self, workspace, review_gate(gate), head, caller),
        )
    }

    fn require_review<'a>(
        &'a self,
        workspace: &'a Workspace,
        gate: ReviewGate,
        head: &'a CommitId,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.reviews
                .require(self, workspace, review_gate(gate), head, caller),
        )
    }

    fn queue_unreviewed(&self, error: anyhow::Error) -> BoxFuture<'_, anyhow::Error> {
        Box::pin(async move {
            let queued = self
                .reviews
                .queue_unreviewed(self, Err::<(), _>(error))
                .await;
            queued.map_or_else(
                |error| error,
                |()| anyhow::anyhow!("a refusal took no review"),
            )
        })
    }

    fn ensure_gate_hears<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.reviews.ensure_gate_hears(self, request))
    }

    fn pending_review<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<CarriedReview>>> {
        Box::pin(self.reviews.pending(id))
    }

    fn queue_carried_review<'a>(
        &'a self,
        id: &'a SpaceId,
        review: CarriedReview,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.reviews.queue_carried(self, id, review))
    }

    fn stop_reviews<'a>(&'a self, workspace_id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.reviews.stop_in_workspace(self, workspace_id))
    }
}
