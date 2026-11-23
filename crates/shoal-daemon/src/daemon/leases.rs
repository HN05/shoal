//! What leases need of the rest of the daemon, which [`Manager`] gives
//! them: the spaces they belong to, their repositories' settings and hooks,
//! the agents that acquire them and the approval requests that gate them.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use shoal_agents::messages::Delivery;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_leases::daemon::{Agents, Approvals, Records, Settings, Sleeper, Spaces, Unborrowable};
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use super::{
    approvals::{self, ApprovalRequest, Target},
    log,
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
        Box::pin(self.spaces.workspace(selector))
    }

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(self.spaces.workspace_by_id(id))
    }

    fn touch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.spaces.touch(id))
    }

    fn verify_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.verify_worktree(self, workspace))
    }

    fn fail_setup<'a>(&'a self, id: &'a SpaceId, message: String) -> BoxFuture<'a, Result<()>> {
        let (ready, failed) = (WorkspaceState::Ready, WorkspaceState::Failed);
        Box::pin(self.spaces.set_state_if(id, ready, failed, Some(message)))
    }
}

impl Settings for Manager {
    fn settings<'a>(
        &'a self,
        config: &'a Config,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(
            self.repositories
                .workspace_settings_from(self, config, workspace),
        )
    }

    fn space_settings<'a>(
        &'a self,
        config: &'a Config,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            let space = self.spaces.space(selector).await?;
            (self.repositories)
                .space_settings_from(self, config, &space, None)
                .await
        })
    }

    fn hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        kind: HookKind,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        Box::pin(self.spaces.workspace_hook(self, workspace, kind))
    }

    fn run_hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        hook: Hook<'a>,
        executable: &'a Path,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let settings = self
                .repositories
                .workspace_settings(self, workspace)
                .await?;
            let repository = workspace.repository();
            let environment =
                (self
                    .spaces
                    .hook_environment(self, workspace, Some(repository), &settings))
                .await?;
            hooks::run_detached(hook, workspace, executable, &self.paths, &environment).await
        })
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(self.repositories.list())
    }
}

impl Agents for Manager {
    fn caller_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<SpaceId>> {
        Box::pin(async move { Some(self.agents.caller_agent(caller).await?.space_id) })
    }

    fn runs_demo<'a>(&'a self, execution: &'a ExecutionId) -> BoxFuture<'a, Result<bool>> {
        let execution = execution.to_owned();
        Box::pin(
            self.store
                .read(move |db| shoal_demos::records::runs_demo(db, &execution)),
        )
    }

    fn recorded_executions(&self) -> BoxFuture<'_, Result<HashSet<ExecutionId>>> {
        Box::pin(self.store.read(|db| recorded_executions(db)))
    }

    fn agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        Box::pin(self.agents.agent_spaces())
    }

    fn has_queued_launch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.agents.has_queued_launch(id))
    }

    fn sleepers<'a>(
        &'a self,
        yielding_to: Option<&'a SpaceId>,
    ) -> BoxFuture<'a, Result<Vec<Sleeper>>> {
        Box::pin(self.sleepers(yielding_to))
    }

    fn tell_agent<'a>(&'a self, id: &'a SpaceId, message: String) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let kind = AlertKind::LeaseTaken;
            let told =
                self.agents
                    .queue_agent_message_delivered(id, kind, message, Delivery::NextTurn);
            if let Err(error) = told.await {
                log!("the agent of {id} was not told of a lease taken back: {error:#}");
            }
        })
    }
}

impl Manager {
    /// The asleep agents, lowest-ranked in the agent queue first, each with
    /// how long its space's settings keep its running leases: every one, or
    /// those whose space's priority is no higher than `yielding_to`'s.
    async fn sleepers(&self, yielding_to: Option<&SpaceId>) -> Result<Vec<Sleeper>> {
        let asleep = self.agents.asleep_agents().await?;
        let mut ids: Vec<SpaceId> = asleep.keys().cloned().collect();
        ids.extend(yielding_to.cloned());
        let ranked = self.agents.ranked_spaces(self, &ids).await?;
        let ceiling = yielding_to.and_then(|requester| {
            let ranks = ranked.iter().find(|(id, _)| id == requester);
            ranks.map(|(_, priority)| *priority)
        });
        let yields = |priority| ceiling.is_none_or(|ceiling| priority <= ceiling);
        let mut sleepers = Vec::with_capacity(ranked.len());
        let asleep_ranked = (ranked.into_iter().rev())
            .filter(|(id, priority)| asleep.contains_key(id) && yields(*priority));
        for (id, _) in asleep_ranked {
            // One removed meanwhile keeps nothing.
            let Ok(space) = self.spaces.space(&SpaceSelector::id(&id)).await else {
                continue;
            };
            let settings = self.repositories.space_settings(self, &space).await?;
            let minutes = settings.sleep.keep_leases_minutes;
            sleepers.push(Sleeper {
                since: asleep[&id],
                keeps_running: Duration::from_secs(minutes.saturating_mul(60)),
                space: id,
            });
        }
        Ok(sleepers)
    }
}

/// Every execution with a record.
fn recorded_executions(db: &Connection) -> Result<HashSet<ExecutionId>> {
    let mut statement = db.prepare("SELECT id FROM executions")?;
    let ids = statement.query_map([], |row| row.get(0))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

impl Approvals for Manager {
    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(self.coordination.notify_approval(self, request))
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
