//! What leases need of the rest of the daemon, which [`Manager`] gives
//! them: the spaces they belong to, their repositories' settings and hooks,
//! the agents that acquire them and the approval requests that gate them.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_leases::daemon::{Agents, Approvals, Records, Settings, Spaces, Unborrowable};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use super::{
    approvals::{self, ApprovalRequest, Target},
    manager::Manager,
    scope::Caller,
    store,
};
use crate::{
    config::{Config, Effective},
    hooks::{self, Hook, HookKind},
    model::{Repository, Workspace},
    state::WorkspaceState,
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

    fn fail_setup<'a>(&'a self, id: &'a SpaceId, message: String) -> BoxFuture<'a, Result<()>> {
        let (ready, failed) = (WorkspaceState::Ready, WorkspaceState::Failed);
        Box::pin(self.set_state_if(id, ready, failed, Some(message)))
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
            let environment =
                (self.hook_environment(workspace, Some(repository), &settings)).await?;
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

    fn recorded_executions(&self) -> BoxFuture<'_, Result<HashMap<ExecutionId, bool>>> {
        Box::pin(self.store.read(|db| recorded_executions(db)))
    }

    fn has_queued_launch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(Manager::has_queued_launch(self, id))
    }
}

/// Every execution with a record, and whether it runs a process.
fn recorded_executions(db: &Connection) -> Result<HashMap<ExecutionId, bool>> {
    let mut statement =
        db.prepare("SELECT id, child IS NOT NULL OR group_id IS NOT NULL FROM executions")?;
    let ids = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

impl Approvals for Manager {
    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(self.notify_approval(request))
    }
}

/// The records of the daemon's other subsystems, as leases read and write
/// them in their own transactions: approval requests, registered
/// repositories and spaces.
pub(super) struct DaemonRecords;

impl Records for DaemonRecords {
    fn check_approval(
        &self,
        tx: &Transaction<'_>,
        request: ApprovalRequest,
    ) -> Result<Option<ApprovalRequest>> {
        approvals::check(tx, request)
    }

    fn release_approval(
        &self,
        tx: &Transaction<'_>,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<bool> {
        approvals::release(tx, owner, target, name)
    }

    fn current_approval(
        &self,
        db: &Connection,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<Option<ApprovalRequest>> {
        approvals::current(db, owner, target, name)
    }

    fn unborrowable(
        &self,
        db: &Connection,
        repository: &Repository,
    ) -> Result<Option<Unborrowable>> {
        let path = repository
            .path
            .to_str()
            .context("repository path is not UTF-8")?;
        let id = &repository.id;
        let registered = "SELECT 1 FROM repositories WHERE id=?1 AND path=?2";
        if !store::exists(db, registered, params![id, path])? {
            return Ok(Some(Unborrowable::Registration));
        }
        let removing = "SELECT 1 FROM repository_removals WHERE repository_id=?1";
        if store::exists(db, removing, [id])? {
            return Ok(Some(Unborrowable::Removal));
        }
        let moving = "SELECT 1 FROM repository_moves WHERE repository_id=?1";
        if store::exists(db, moving, [id])? {
            return Ok(Some(Unborrowable::Move));
        }
        let worktree = "SELECT 1 FROM workspaces WHERE path=?1";
        Ok(store::exists(db, worktree, [path])?.then_some(Unborrowable::Worktree))
    }

    fn space_name(&self, db: &Connection, id: &SpaceId) -> Result<Option<SpaceName>> {
        super::spaces::space_name(db, id)
    }

    fn space_repository(&self, db: &Connection, id: &SpaceId) -> Result<Option<RepositoryId>> {
        let repository = "SELECT repository_id FROM spaces WHERE id=?1";
        let found = db
            .query_row(repository, [id], |row| row.get(0))
            .optional()?;
        Ok(found.flatten())
    }
}

#[cfg(test)]
mod tests;
