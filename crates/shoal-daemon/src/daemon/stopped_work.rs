//! What a stop saved for `shoal restore`: keeping stopped agents as last
//! sessions after `shoal stop`, and forgetting them, which the daemon does so
//! it can confine coordinating agents to the workspaces below theirs.
use anyhow::{Context as _, Result, ensure};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
pub use shoal_protocol::executions::Forgotten;

use super::manager::Manager;
use crate::daemon::scope::Caller;
use crate::execution::recovery::{self, Saved, SavedExt as _};
use crate::model::Space;
use crate::protocol::SavedAgent;
use shoal_protocol::saved::SavedSummary;
use shoal_protocol::space_names::SpaceSelector;

/// What a change to the permissions given applies to: the stopped agents of
/// workspaces and chats, which restores run under, and the workspaces whose
/// failed setup a launch waits for, which it starts under.
pub struct PermissionTargets<'a> {
    pub agents: &'a [SavedAgent],
    pub launches: &'a [SpaceSelector],
}

impl Manager {
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
        Saved::load(&self.paths, &space.id)
    }

    /// What each space `caller` sees saved, in brief, for those that saved
    /// anything. One whose state cannot be read counts as saving nothing, so
    /// it costs only its own space.
    pub(crate) async fn saved_summaries(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<SavedSummary>> {
        let spaces = self.visible_spaces(caller).await?;
        let summary = |space: Space| {
            let saved = Saved::load(&self.paths, &space.id).ok()?;
            (!saved.is_empty()).then(|| saved.summary(space.id))
        };
        Ok(spaces.into_iter().filter_map(summary).collect())
    }

    /// Keep the space's stopped agents whose execution ended as last
    /// sessions, which hold no heavy slot and nothing restores on its own.
    /// One a wrapper waits to restore stays, and one that cannot be kept,
    /// such as one a resume holds, stays stopped work.
    pub(in crate::daemon) async fn keep_stopped_as_last_sessions(
        &self,
        selector: &SpaceSelector,
    ) -> Result<()> {
        let space = self.space(selector).await?;
        let executions = self.workspace_executions(&space.id).await?;
        let mut saved = Saved::load(&self.paths, &space.id)?;
        saved.retain_unattended();
        for recovery::SavedSession { id, path, .. } in saved.agents {
            if executions.iter().any(|execution| execution.id == id) {
                continue;
            }
            if let Err(error) = recovery::keep_as_last_session(&self.paths, &space.id, &id, &path) {
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
        let mut saved = Saved::load(&self.paths, &space.id)?;
        if let Some(execution) = &execution {
            saved.retain_execution(execution, false);
        }
        ensure!(
            !saved.is_empty(),
            "no stopped agents or commands for this {}",
            space.kind()
        );
        let executions = self.workspace_executions(&space.id).await?;
        let active = |id: &ExecutionId| executions.iter().any(|e| e.id == *id);
        let executions = recovery::forget_saved(&saved, active)?;
        // The saved sessions are files, so no stored change announces it.
        self.signals.agent_changed();
        Ok(Forgotten {
            space: space.name.clone(),
            executions,
        })
    }

    /// Make `change` to the permissions each of the stopped agents and
    /// waiting launches `targets` names was given, once none of them is
    /// refused, for the scoped process `giver`, which gives only what it
    /// holds, or for the user.
    pub async fn change_stopped_permissions(
        &self,
        targets: PermissionTargets<'_>,
        change: &crate::config::permissions::PermissionChange,
        giver: Option<&Caller>,
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
        let (failed, waiting): (Vec<_>, Vec<_>) = (launches.into_iter())
            .map(|launch| (launch.state, launch.id, change.applied_to(launch.narrowing)))
            .partition(|(state, ..)| *state == crate::state::QueueState::Failed);
        let given = |launches: Vec<_>| (launches.into_iter()).map(|(_, id, given)| (id, given));
        (self.give_launches(given(waiting).collect(), given(failed).collect())).await?;
        claimed.make(change)
    }

    /// The launch waiting for the failed setup of the workspace `selector`
    /// names, else the space's failed launch, refusing a space without
    /// either and `change` where `giver` does not hold what it gives, as
    /// [`Manager::ensure_change_held`] refuses it.
    async fn changeable_launch(
        &self,
        selector: &SpaceSelector,
        change: &crate::config::permissions::PermissionChange,
        giver: Option<&Caller>,
    ) -> Result<crate::model::QueueEntry> {
        let space = self.space(selector).await?;
        let repository = space.repository_id.clone();
        (self.ensure_change_held(giver, &space, change, repository.as_ref())).await?;
        if let Some(launch) = self.launch_awaiting_setup(&space.id).await? {
            return Ok(launch);
        }
        let failed = self.failed_launch_of(&space.id).await?;
        failed.with_context(|| {
            format!(
                "no agent of {} waits for its setup or a restore of its failed start",
                space.name
            )
        })
    }

    /// What the stopped `agent` saved, refusing one that saved nothing and
    /// `change` where `giver` does not hold what it gives, as
    /// [`Manager::ensure_change_held`] refuses it. A wrapper waiting to
    /// restore the agent restores it under the changed record.
    async fn changeable(
        &self,
        agent: &SavedAgent,
        change: &crate::config::permissions::PermissionChange,
        giver: Option<&Caller>,
    ) -> Result<Saved> {
        let (space, execution) = (self.space(&agent.workspace).await?, &agent.execution);
        let repository = space.repository_id.as_ref().cloned();
        (self.ensure_change_held(giver, &space, change, repository.as_ref())).await?;
        let mut saved = Saved::load(&self.paths, &space.id)?;
        saved.retain_execution(execution, false);
        ensure!(
            !(saved.agents.is_empty() && saved.sessions.is_empty()),
            "{} has no stopped agent {execution}",
            space.name
        );
        Ok(saved)
    }
}

#[cfg(test)]
mod tests;
