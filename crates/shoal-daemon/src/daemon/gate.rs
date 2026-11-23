//! What the gate needs of the rest of the daemon, which [`Manager`] gives
//! it: the spaces whose approval requests it decides, the approvals that
//! carry out its verdicts and the requests' records, which it changes in
//! the transactions that record its verdicts.
use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_daemon_base::notifications::About;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::config::permissions::Permissions;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::views::Viewed;
use shoal_reviews::daemon::{Approvals, Requesters, Requests};

use super::{
    approvals::{ApprovalRequest, by_key, gated, proposals::ProposalDecision},
    manager::Manager,
    scope::Caller,
};
use crate::{forge::items::link::Selection, model::Space, protocol::ItemContext};

impl Requesters for Manager {
    fn space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        Box::pin(self.space_by_id(id))
    }

    fn view_items<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Viewed>> {
        Box::pin(async move {
            let target = ItemContext::space(SpaceSelector::id(id)).into();
            let selection = Selection {
                kind: None,
                input: None,
            };
            self.links
                .view_items(self, &target, &selection, false, None)
                .await
        })
    }

    fn permissions<'a>(
        &'a self,
        id: &'a SpaceId,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Permissions>> {
        Box::pin(self.coordination.space_permissions(self, id, repository))
    }

    fn tell_space<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, bool> {
        Box::pin(self.coordination.tell_space(self, id, kind, message))
    }

    fn notify(&self, about: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()> {
        Box::pin(self.coordination.notify_about(self, about, kind, message))
    }

    fn issue_scope(&self, caller: &Caller) -> ScopeToken {
        self.operations.issue(caller)
    }

    fn revoke_scope(&self, token: &ScopeToken) {
        self.operations.revoke(token);
    }
}

impl Approvals for Manager {
    fn approve<'a>(&'a self, allowed: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let shown = Some(allowed.clone());
            let details = ProposalDecision::default();
            (self
                .coordination
                .decide_approval(self, allowed.number, true, shown, details, None))
            .await?;
            Ok(())
        })
    }

    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(self.coordination.notify_approval(self, request))
    }

    fn prune_proposal_files<'a>(&'a self, denied: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(self.links.prune_proposal_files(self, denied))
    }
}

/// The approval requests' records, as the gate reads and changes them in
/// its own transactions.
pub(crate) struct GatedRequests;

impl Requests for GatedRequests {
    fn request(&self, db: &Connection, key: &str) -> Result<Option<ApprovalRequest>> {
        by_key(db, key)
    }

    fn give(&self, tx: &Transaction<'_>, key: &str) -> Result<bool> {
        gated::give_gate(tx, key)
    }

    fn deciding(&self, db: &Connection) -> Result<Vec<ApprovalRequest>> {
        gated::deciding(db)
    }

    fn stranded(&self, db: &Connection) -> Result<Vec<ApprovalRequest>> {
        gated::stranded(db)
    }

    fn allow(&self, tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest> {
        gated::allow(tx, key)
    }

    fn deny(&self, tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest> {
        gated::deny(tx, key)
    }

    fn escalate(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        reason: &str,
        rule: &str,
    ) -> Result<ApprovalRequest> {
        gated::escalate(tx, key, reason, rule)
    }

    fn unjudged(&self, tx: &Transaction<'_>, key: &str, reason: &str) -> Result<ApprovalRequest> {
        gated::unjudged(tx, key, reason)
    }

    fn unapplied(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        failure: &str,
    ) -> Result<Option<ApprovalRequest>> {
        gated::unapplied(tx, key, failure)
    }
}

#[cfg(test)]
mod tests;
