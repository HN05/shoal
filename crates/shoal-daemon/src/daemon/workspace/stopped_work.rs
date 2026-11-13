//! What a stop saved for `shoal restore`: keeping stopped agents as last
//! sessions after `shoal stop`, and forgetting them, which the daemon does so
//! it can confine coordinating agents to the workspaces below theirs.
use crate::client::execution_ids::ExecutionId;
pub use crate::client::executions::Forgotten;
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result, ensure};

use super::Manager;
use crate::client::saved::SavedSummary;
use crate::client::space_names::SpaceSelector;
use crate::execution::recovery::{self, Saved, SavedExt as _};
use crate::protocol::SavedAgent;

/// A workspace process that stopped another workspace's executions, which
/// may resume the agents among them.
pub(super) struct Stopper<'a> {
    /// The ID of the process's space.
    pub(super) space: &'a SpaceId,
    /// The executions the stop ended.
    pub(super) executions: Vec<ExecutionId>,
}

/// What a change to the permissions given applies to: the stopped agents of
/// workspaces and chats, which restores run under, and the workspaces whose
/// failed setup a launch waits for, which it starts under.
pub(crate) struct PermissionTargets<'a> {
    pub agents: &'a [SavedAgent],
    pub launches: &'a [SpaceSelector],
}

impl Manager {
    /// The executions of workspace `id` that nothing stopped yet. An agent
    /// that already saved its recovery record was stopped before, such as
    /// one whose wrapper waits to restore it after a protection stop.
    pub(super) async fn unstopped_executions(&self, id: &SpaceId) -> Result<Vec<ExecutionId>> {
        let executions = self.workspace_executions(id).await?;
        let stopped_before =
            |execution: &ExecutionId| recovery::record_path(&self.paths, id, execution).exists();
        Ok(executions
            .into_iter()
            .map(|execution| execution.id)
            .filter(|execution| !stopped_before(execution))
            .collect())
    }

    /// Whether a stop saved an agent of workspace `id`: one a wrapper waits
    /// to restore, stopped work or a last session, which `shoal restore`
    /// restores. A state directory that cannot be read counts as none, so
    /// it costs only its own workspace.
    pub(crate) fn has_saved_agent(&self, id: &SpaceId) -> bool {
        recovery::saves_agent(&self.paths, id).unwrap_or_else(|error| {
            crate::daemon::log!("saved agents of workspace {id} unreadable: {error:#}");
            false
        })
    }

    /// What the existing space `selector` names saved for `shoal restore`.
    pub(crate) async fn saved_work(&self, selector: &SpaceSelector) -> Result<Saved> {
        let space = self.space(selector).await?;
        Saved::load(&self.paths, space.id())
    }

    /// What each workspace and chat visible within `owner`'s reach, or every
    /// one, saved, in brief, for those that saved anything. One whose state
    /// cannot be read counts as saving nothing, so it costs only its own
    /// space.
    pub(crate) async fn saved_summaries(
        &self,
        owner: Option<&SpaceId>,
    ) -> Result<Vec<SavedSummary>> {
        let workspaces = self.visible_workspaces(owner).await?;
        let mut spaces: Vec<SpaceId> = workspaces.into_iter().map(|w| w.id).collect();
        spaces.extend(self.chats_within(owner).await?);
        let summary = |id: SpaceId| {
            let saved = Saved::load(&self.paths, &id).ok()?;
            (!saved.is_empty()).then(|| saved.summary(id))
        };
        Ok(spaces.into_iter().filter_map(summary).collect())
    }

    /// The IDs of the chats within `owner`'s reach, or of every one.
    async fn chats_within(&self, owner: Option<&SpaceId>) -> Result<Vec<SpaceId>> {
        let reached = match owner {
            Some(owner) => Some(self.subtree(owner).await?),
            None => None,
        };
        let chats: Vec<SpaceId> = self
            .store
            .run(|db| {
                Ok(db
                    .prepare("SELECT id FROM chats")?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;
        Ok(match reached {
            Some(reached) => chats
                .into_iter()
                .filter(|id| reached.contains(id))
                .collect(),
            None => chats,
        })
    }

    /// Keep the space's stopped agents whose execution ended as last
    /// sessions, which hold no agent slot and nothing restores on its own,
    /// recording `stopper` on those it stopped. One a wrapper waits to
    /// restore stays, and one that cannot be kept, such as one a resume
    /// holds, stays stopped work.
    pub(super) async fn keep_stopped_as_last_sessions(
        &self,
        selector: &SpaceSelector,
        stopper: Option<&Stopper<'_>>,
    ) -> Result<()> {
        let space = self.space(selector).await?;
        let executions = self.workspace_executions(space.id()).await?;
        let mut saved = Saved::load(&self.paths, space.id())?;
        saved.retain_unattended();
        for recovery::SavedSession { id, path, .. } in saved.agents {
            if executions.iter().any(|execution| execution.id == id) {
                continue;
            }
            let stopped_by = stopper
                .filter(|stopper| stopper.executions.contains(&id))
                .map(|stopper| stopper.space);
            if let Err(error) =
                recovery::keep_as_last_session(&self.paths, space.id(), &id, &path, stopped_by)
            {
                crate::daemon::log!("stopped agent {id} not kept as a last session: {error:#}");
            }
        }
        Ok(())
    }

    /// Forget the space's stopped agents, last sessions and commands, only
    /// the one of `execution` when given.
    pub async fn forget_stopped_work(
        &self,
        selector: &SpaceSelector,
        execution: Option<ExecutionId>,
    ) -> Result<Forgotten> {
        let space = self.space(selector).await?;
        let mut saved = Saved::load(&self.paths, space.id())?;
        if let Some(execution) = &execution {
            saved.retain_execution(execution, false);
        }
        ensure!(
            !saved.is_empty(),
            "no stopped agents or commands for this {}",
            space.kind()
        );
        let executions = self.workspace_executions(space.id()).await?;
        let active = |id: &ExecutionId| executions.iter().any(|e| e.id == *id);
        let executions = recovery::forget_saved(&saved, active)?;
        // The saved sessions are files, so no stored change announces it.
        self.signals.agent_changed();
        Ok(Forgotten {
            space: space.name().clone(),
            executions,
        })
    }

    /// Make `change` to the permissions each of the stopped agents and
    /// waiting launches `targets` names was given, once none of them is refused, for
    /// the scoped process of the space `giver`, which gives only what it
    /// holds, or for the user.
    pub async fn change_stopped_permissions(
        &self,
        targets: PermissionTargets<'_>,
        change: &crate::config::permissions::PermissionChange,
        giver: Option<&SpaceId>,
    ) -> Result<()> {
        let mut changed = Vec::new();
        for agent in targets.agents {
            changed.push(self.changeable(agent, change, giver).await?);
        }
        let mut launches = Vec::new();
        for workspace in targets.launches {
            launches.push(self.changeable_launch(workspace, change, giver).await?);
        }
        // The launches change together, refusing one started or removed
        // since, while the agents' claims keep their restores out.
        let claimed = recovery::ClaimedChange::claim(&changed)?;
        let given = launches
            .into_iter()
            .map(|launch| (launch.id, change.applied_to(launch.narrowing)));
        self.give_launches_awaiting_setup(given.collect()).await?;
        claimed.make(change)
    }

    /// The launch waiting for the failed setup of the workspace `selector`
    /// names, refusing a workspace without one and `change` where `giver`
    /// does not hold what it gives, as [`Manager::ensure_change_held`]
    /// refuses it.
    async fn changeable_launch(
        &self,
        selector: &SpaceSelector,
        change: &crate::config::permissions::PermissionChange,
        giver: Option<&SpaceId>,
    ) -> Result<crate::model::QueueEntry> {
        let workspace = self.workspace(selector).await?;
        let repository = Some(workspace.repository_id.clone());
        let space = crate::model::PresentSpace::Workspace(workspace.clone());
        (self.ensure_change_held(giver, &space, change, repository.as_ref())).await?;
        let launch = self.launch_awaiting_setup(&workspace.id).await?;
        launch.with_context(|| format!("no agent waits for the setup of {}", workspace.name))
    }

    /// What the stopped `agent` saved, refusing one that saved nothing, one
    /// its waiting wrapper restores, which keeps the permissions that
    /// wrapper launched it with, and `change` where `giver` does not hold
    /// what it gives, as [`Manager::ensure_change_held`] refuses it.
    async fn changeable(
        &self,
        agent: &SavedAgent,
        change: &crate::config::permissions::PermissionChange,
        giver: Option<&SpaceId>,
    ) -> Result<Saved> {
        let (space, execution) = (self.space(&agent.workspace).await?, &agent.execution);
        let repository = space.repository_id().cloned();
        (self.ensure_change_held(giver, &space, change, repository.as_ref())).await?;
        let mut saved = Saved::load(&self.paths, space.id())?;
        saved.retain_execution(execution, false);
        ensure!(
            !(saved.agents.is_empty() && saved.sessions.is_empty()),
            "{} has no stopped agent {execution}",
            space.name()
        );
        ensure!(
            !saved.agents.iter().any(|agent| agent.waits_in_wrapper),
            "the agent of {} restores in the wrapper that waits for it, which keeps its permissions; restore it without --permission",
            space.name()
        );
        Ok(saved)
    }
}

#[cfg(test)]
mod tests;
