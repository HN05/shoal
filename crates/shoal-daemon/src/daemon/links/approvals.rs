//! The approval requests proposals and pushes wait for, the operations
//! that run approved pushes, and the reviews a new PR and a PR merge wait
//! for, as links reach them.
use std::collections::BTreeMap;

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use shoal_ids::space_ids::SpaceId;
use shoal_links::daemon::{Approvals, Reviews};
use shoal_links::pr::current_head;
use shoal_refs::CommitId;
use shoal_reviews::required::{Gate, Unreviewed};

use crate::{
    config::Effective,
    daemon::{
        alerts::{About, AlertKind},
        approvals::ApprovalRequest,
        manager::Manager,
        scope::Caller,
    },
    forge::items::reference::ItemRef,
    model::Workspace,
    protocol::operations::OperationSpec,
};

impl Approvals for Manager {
    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(self.coordination.notify_approval(self, request))
    }

    fn ensure_gate_hears<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.reviews.ensure_gate_hears(self, request))
    }

    fn tell_space<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, bool> {
        Box::pin(self.coordination.tell_space(self, id, kind, message))
    }

    fn notify_about(&self, about: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()> {
        Box::pin(self.coordination.notify_about(self, about, kind, message))
    }

    fn start_operation(
        &self,
        spec: OperationSpec,
        env: Option<BTreeMap<String, String>>,
    ) -> BoxFuture<'_, Result<String>> {
        Box::pin(self.spaces.start_operation(spec, env, None))
    }
}

impl Reviews for Manager {
    fn merge_review_required(&self, settings: &Effective, caller: Option<&Caller>) -> bool {
        Gate::Merge.holds(settings, caller)
    }

    fn require_merge_review<'a>(
        &'a self,
        owner: &'a Workspace,
        head: &'a CommitId,
        url: &'a ItemRef,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let checked = (self.reviews)
                .check_reviewed(owner, Gate::Merge, head)
                .await;
            self.queue_pull_review(checked, owner, head, url, caller)
                .await
        })
    }

    fn pr_review_required(&self, settings: &Effective, caller: Option<&Caller>) -> bool {
        Gate::Pr.holds(settings, caller)
    }

    fn require_pr_review<'a>(
        &'a self,
        workspace: &'a Workspace,
        head: &'a CommitId,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.reviews
                .require(self, workspace, Gate::Pr, head, caller),
        )
    }
}

impl Manager {
    /// `checked`, the gate of `owner` at `head`, with a review queued for an
    /// [`Unreviewed`] refusal where that review covers `head`, as a review
    /// of the owner's HEAD does when it is `head`, and `caller`'s scope
    /// reaches the owner; otherwise the refusal names how to review the PR.
    async fn queue_pull_review(
        &self,
        checked: Result<()>,
        owner: &Workspace,
        head: &CommitId,
        url: &ItemRef,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Err(error) = checked else {
            return Ok(());
        };
        if error.downcast_ref::<Unreviewed>().is_none() {
            return Err(error);
        }
        let at_head = current_head(owner).await.ok().as_ref() == Some(head);
        let reached = match caller {
            Some(caller) => self.coordination.reaches(self, caller, &owner.id).await?,
            None => true,
        };
        match at_head && reached {
            true => self.reviews.queue_unreviewed(self, Err(error)).await,
            false => bail!("{error}; shoal review run {url} reviews the PR"),
        }
    }
}
