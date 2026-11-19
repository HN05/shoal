//! What leases need of the rest of the daemon, which [`Manager`] gives
//! them: the spaces they belong to, their repositories' settings and the
//! approval requests that gate them.
use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::Transaction;
use shoal_ids::space_ids::SpaceId;
use shoal_leases::daemon::{Approvals, Grants, Settings, Spaces};
use shoal_protocol::space_names::SpaceSelector;

use super::{
    approvals::{self, ApprovalRequest, Target},
    manager::Manager,
};
use crate::{
    config::{Config, Effective},
    model::Workspace,
};

impl Spaces for Manager {
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(Manager::workspace(self, selector))
    }

    fn touch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(Manager::touch(self, id))
    }
}

impl Settings for Manager {
    fn settings<'a>(
        &'a self,
        config: &'a Config,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.workspace_settings_from(config, workspace))
    }
}

impl Approvals for Manager {
    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(self.notify_approval(request))
    }
}

/// The approval requests the daemon records, as leases check and release
/// them.
pub(super) struct ApprovalGrants;

impl Grants for ApprovalGrants {
    fn check(
        &self,
        tx: &Transaction<'_>,
        request: ApprovalRequest,
    ) -> Result<Option<ApprovalRequest>> {
        approvals::check(tx, request)
    }

    fn release(
        &self,
        tx: &Transaction<'_>,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<bool> {
        approvals::release(tx, owner, target, name)
    }
}

#[cfg(test)]
mod tests;
