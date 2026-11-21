//! The approval requests proposals and pushes wait for, the operations
//! that run approved pushes, and the reviews a new PR waits for, as links
//! reach them.
use std::collections::BTreeMap;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::space_ids::SpaceId;
use shoal_links::daemon::{Approvals, Reviews};
use shoal_refs::CommitId;
use shoal_reviews::required::Gate;

use crate::{
    config::Effective,
    daemon::{
        alerts::{About, AlertKind},
        approvals::ApprovalRequest,
        manager::Manager,
        scope::Caller,
    },
    model::Workspace,
    protocol::operations::OperationSpec,
};

impl Approvals for Manager {
    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(self.notify_approval(request))
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
        Box::pin(Manager::tell_space(self, id, kind, message))
    }

    fn notify_about(&self, about: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()> {
        Box::pin(Manager::notify_about(self, about, kind, message))
    }

    fn start_operation(
        &self,
        spec: OperationSpec,
        env: Option<BTreeMap<String, String>>,
    ) -> BoxFuture<'_, Result<String>> {
        Box::pin(Manager::start_operation(self, spec, env, None))
    }

    fn operation_running(&self, id: &str) -> bool {
        Manager::operation_running(self, id)
    }
}

impl Reviews for Manager {
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
