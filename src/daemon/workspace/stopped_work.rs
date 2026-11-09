//! What a stop saved for `shoal resume`: keeping stopped agents as last
//! sessions after `shoal stop`, and forgetting them, which the daemon does so
//! it can confine coordinating agents to the workspaces below theirs.
use crate::client::execution_ids::ExecutionId;
pub use crate::client::executions::Discarded;
use crate::client::space_ids::SpaceId;
use anyhow::{Result, ensure};

use super::Manager;
use crate::client::space_names::SpaceSelector;
use crate::execution::recovery::{self, Saved};
use crate::protocol::SavedAgent;

/// A workspace process that stopped another workspace's executions, which
/// may resume the agents among them.
pub(super) struct Stopper<'a> {
    /// The ID of the process's workspace.
    pub(super) workspace: &'a SpaceId,
    /// The executions the stop ended.
    pub(super) executions: Vec<ExecutionId>,
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
    /// to restore, stopped work or a last session, which `shoal resume`
    /// restores. A state directory that cannot be read counts as none, so
    /// it costs only its own workspace.
    pub(crate) fn has_saved_agent(&self, id: &SpaceId) -> bool {
        recovery::saves_agent(&self.paths, id).unwrap_or_else(|error| {
            crate::daemon::log!("saved agents of workspace {id} unreadable: {error:#}");
            false
        })
    }

    /// Keep the workspace's stopped agents whose execution ended as last
    /// sessions, which hold no agent slot and nothing restores on its own,
    /// recording `stopper` on those it stopped. One a wrapper waits to
    /// restore stays, and one that cannot be kept, such as one a resume
    /// holds, stays stopped work.
    pub(super) async fn keep_stopped_as_last_sessions(
        &self,
        selector: &SpaceSelector,
        stopper: Option<&Stopper<'_>>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        let executions = self.workspace_executions(&workspace.id).await?;
        let mut saved = Saved::load(&self.paths, &workspace.id)?;
        saved.retain_unattended();
        for (id, path) in saved.agents {
            if executions.iter().any(|execution| execution.id == id) {
                continue;
            }
            let stopped_by = stopper
                .filter(|stopper| stopper.executions.contains(&id))
                .map(|stopper| stopper.workspace);
            if let Err(error) =
                recovery::keep_as_last_session(&self.paths, &workspace.id, &id, &path, stopped_by)
            {
                crate::daemon::log!("stopped agent {id} not kept as a last session: {error:#}");
            }
        }
        Ok(())
    }

    /// Forget the workspace's stopped agents, last sessions and commands, only
    /// the one of `execution` when given.
    pub async fn discard_stopped_work(
        &self,
        selector: &SpaceSelector,
        execution: Option<ExecutionId>,
    ) -> Result<Discarded> {
        let workspace = self.workspace(selector).await?;
        let mut saved = Saved::load(&self.paths, &workspace.id)?;
        if let Some(execution) = &execution {
            saved.retain_execution(execution, false);
        }
        ensure!(
            !saved.is_empty(),
            "no stopped agents or commands for this workspace"
        );
        let executions = self.workspace_executions(&workspace.id).await?;
        let active = |id: &ExecutionId| executions.iter().any(|e| e.id == *id);
        let executions = recovery::discard_saved(&saved, active)?;
        // The saved sessions are files, so no stored change announces it.
        self.signals.agent_changed();
        Ok(Discarded {
            workspace: workspace.name.into(),
            executions,
        })
    }

    /// Make `change` to the permissions each of the stopped `agents` and
    /// saved `chats` was given, which its restore runs under, once none of
    /// them is refused, for the scoped process of the space `giver`, which
    /// gives only what it holds, or for the user.
    pub async fn change_stopped_permissions(
        &self,
        agents: &[SavedAgent],
        chats: &[SpaceSelector],
        change: &crate::config::permissions::PermissionChange,
        giver: Option<&SpaceId>,
    ) -> Result<()> {
        let mut changed = Vec::new();
        for agent in agents {
            changed.push(self.changeable(agent, change, giver).await?);
        }
        let mut saved = Vec::new();
        for chat in chats {
            saved.push(self.changeable_chat(chat, change, giver).await?);
        }
        recovery::change_permissions(&changed, change)?;
        for chat in saved {
            (self.give_saved_chat(&chat.id, change.applied_to(chat.narrowing))).await?;
        }
        Ok(())
    }

    /// The saved chat `selector` names, refusing one that is not saved and
    /// `change` where `giver` does not hold what it gives.
    async fn changeable_chat(
        &self,
        selector: &SpaceSelector,
        change: &crate::config::permissions::PermissionChange,
        giver: Option<&SpaceId>,
    ) -> Result<crate::model::Chat> {
        let chat = self.chat(selector).await?;
        ensure!(chat.restore.is_some(), "chat {} is not stopped", chat.name);
        let repository = chat.repository_id.as_deref();
        self.ensure_caller_holds(giver, &change.set, repository)
            .await?;
        Ok(chat)
    }

    /// What the stopped `agent` saved, refusing one that saved nothing, one
    /// its waiting wrapper restores, which keeps the permissions that
    /// wrapper launched it with, and `change` where `giver` does not hold
    /// what it gives.
    async fn changeable(
        &self,
        agent: &SavedAgent,
        change: &crate::config::permissions::PermissionChange,
        giver: Option<&SpaceId>,
    ) -> Result<Saved> {
        let (workspace, execution) = (self.workspace(&agent.workspace).await?, &agent.execution);
        let repository = Some(workspace.repository_id.as_str());
        self.ensure_caller_holds(giver, &change.set, repository)
            .await?;
        let mut saved = Saved::load(&self.paths, &workspace.id)?;
        saved.retain_execution(execution, false);
        ensure!(
            !(saved.agents.is_empty() && saved.sessions.is_empty()),
            "{} has no stopped agent {execution}",
            workspace.name
        );
        ensure!(
            !saved
                .agents
                .iter()
                .any(|(_, path)| recovery::waits_in_wrapper(path)),
            "the agent of {} restores in the wrapper that waits for it, which keeps its permissions; resume it without --permission",
            workspace.name
        );
        Ok(saved)
    }
}

#[cfg(test)]
mod tests;
