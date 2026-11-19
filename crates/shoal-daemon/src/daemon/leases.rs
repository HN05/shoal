//! What leases need of the rest of the daemon, which [`Manager`] gives
//! them: the spaces they belong to, their repositories' settings and hooks,
//! the agents that acquire them and the approval requests that gate them.
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_leases::daemon::{Agents, Approvals, Grants, Settings, Spaces};
use shoal_protocol::space_names::SpaceSelector;

use super::{
    approvals::{self, ApprovalRequest, Target},
    manager::Manager,
    scope::Caller,
};
use crate::{
    config::{Config, Effective},
    hooks::{self, Hook, HookKind},
    model::{Repository, Workspace},
};

impl Spaces for Manager {
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(Manager::workspace(self, selector))
    }

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(Manager::workspace_by_id(self, id))
    }

    fn touch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(Manager::touch(self, id))
    }

    fn verify_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::verify_worktree(self, workspace))
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

    fn hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        kind: HookKind,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        Box::pin(self.workspace_hook(workspace, kind))
    }

    fn run_hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        hook: Hook<'a>,
        executable: &'a Path,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let settings = self.workspace_settings(workspace).await?;
            let repository = workspace.repository();
            let environment = (self.hook_environment(workspace, repository, &settings)).await?;
            hooks::run_detached(hook, workspace, executable, &self.paths, &environment).await
        })
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(Manager::repositories(self))
    }
}

impl Agents for Manager {
    fn caller_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<ExecutionId>> {
        Box::pin(async move { Some(Manager::caller_agent(self, caller).await?.execution) })
    }

    fn runs_demo<'a>(&'a self, execution: &'a ExecutionId) -> BoxFuture<'a, Result<bool>> {
        let execution = execution.to_owned();
        Box::pin(
            self.store
                .read(move |db| super::demos::runs_demo(db, &execution)),
        )
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

    fn current(
        &self,
        db: &Connection,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<Option<ApprovalRequest>> {
        approvals::current(db, owner, target, name)
    }
}

#[cfg(test)]
mod tests;
