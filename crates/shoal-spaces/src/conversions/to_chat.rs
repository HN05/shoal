//! A workspace becoming a chat once its agent's turn ends, which removes
//! the worktree and restarts the agent in the repository's checkout.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, bail, ensure};

use super::pending::{Carrying, ConvertedChat};
use crate::{
    config::modes::ModeName,
    model::{CheckpointCause, HoldPlacer, SpaceKind, Workspace},
    protocol::AgentChange,
    removal::InspectionPolicy,
    scope::Caller,
    target_checks,
};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    /// The agent carries on as `change` asks for `caller`. The checks of the
    /// removal the conversion makes then come first.
    pub async fn convert_to_chat(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        mode: ModeName,
        change: AgentChange,
        caller: Option<&Caller>,
    ) -> Result<Workspace> {
        let workspace = self.convertible_workspace(daemon, selector).await?;
        // The chat runs in the checkout, under its settings, not the
        // worktree's.
        let settings = (daemon.repository_settings_with_templates(workspace.repository())).await?;
        super::ensure_mode(&settings, &mode)?;
        let current = self.agent_of(&workspace.id).await?;
        let place = (&settings, (SpaceKind::Chat, true));
        let narrowing = workspace.narrowing;
        let taking =
            super::to_agent::switching(narrowing, place, &mode, &change, current.as_ref())?;
        let carried = (taking.as_ref(), &change);
        (self.ensure_carrying_held(daemon, &workspace.id, carried, caller)).await?;
        let carrying = Carrying {
            model: change.model.clone(),
            taking: taking.map(Box::new),
        };
        // No agent starts in the workspace from when the one that carries on
        // is chosen.
        let _admissions = daemon.lock_agent_admissions().await;
        let chat = (self.carrying_on(daemon, &workspace, mode, &change, &carrying)).await?;
        let id = workspace.id.clone();
        self.store
            .write(move |tx| super::pending::record_chat_conversion(tx, &id, chat, carrying))
            .await?;
        // The relocation waits for the agent's turn to end, which it may
        // have already.
        self.signals.agent_changed();
        Ok(workspace)
    }

    /// Its running agent, with the permissions `change` gives it, carries on
    /// in it as `carrying` says, or else an agent tool `carrying` names takes
    /// over the work of the agent it saved last.
    async fn carrying_on(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        mode: ModeName,
        change: &AgentChange,
        carrying: &Carrying,
    ) -> Result<ConvertedChat> {
        let running = self.running_agent(daemon, &workspace.id).await?;
        if let Some(execution) = running {
            let chat = self.converted_chat(&workspace.id, mode, execution).await?;
            return Ok(ConvertedChat {
                narrowing: change.permissions.applied_to(chat.narrowing),
                ..chat
            });
        }
        let taking = (carrying.taking.as_ref()).with_context(|| {
            format!(
                "no agent runs in {} to carry on in a chat; start one first",
                workspace.name
            )
        })?;
        // Not one that waits in the agent queue, which the handoff could not
        // replace once the worktree is gone.
        let space = crate::model::Space::from(workspace.clone());
        daemon.ensure_replaceable(&space).await?;
        let saved = crate::spaces::sessions::newest_saved(&self.paths, &workspace.id)?;
        Ok(ConvertedChat {
            agent: (saved.as_ref().map(|saved| saved.agent.clone()))
                .unwrap_or_else(|| taking.agent.clone()),
            execution: (saved.as_ref().map(|saved| saved.execution.clone()))
                .unwrap_or_else(ExecutionId::generate),
            origin: saved.and_then(|saved| saved.origin),
            mode,
            narrowing: taking.narrowing,
        })
    }

    /// Refuses while the space converts: a workspace that becomes a chat,
    /// whose agent carries on there; a chat that became it, whose agent's
    /// turn end, which another agent's reports would hide, moves it there;
    /// or a space whose work goes over to another agent tool. The caller
    /// holds the agents' admissions.
    pub async fn ensure_takes_agents(&self, selector: &SpaceSelector) -> Result<()> {
        let space = self.space(selector).await?;
        let id = space.id.clone();
        let pending = self.store.run(move |db| super::pending(db, &id)).await?;
        let (kind, name) = (space.kind(), &space.name);
        match pending {
            Some(super::Pending::Chat { .. }) => bail!(
                "workspace {name} becomes a chat once its agent's turn ends; restore the agent in the chat then"
            ),
            Some(super::Pending::Workspace { .. }) => bail!(
                "the agent of the chat that became workspace {name} moves there once its turn ends; start another agent there then"
            ),
            Some(super::Pending::Agent { taking, .. }) => bail!(
                "the work of {kind} {name} goes over to {}; start another agent there once it has",
                taking.agent
            ),
            None => Ok(()),
        }
    }

    /// Its agent runs as `execution`.
    async fn converted_chat(
        &self,
        id: &SpaceId,
        mode: ModeName,
        execution: ExecutionId,
    ) -> Result<ConvertedChat> {
        let recorded = self
            .workspace_executions(id)
            .await?
            .into_iter()
            .find(|recorded| recorded.id == execution)
            .context("the agent to carry on is gone")?;
        let agent = recorded
            .agent
            .context("the execution to carry on runs no agent")?;
        Ok(ConvertedChat {
            agent,
            origin: recorded.origin,
            mode,
            narrowing: recorded.narrowing,
            execution,
        })
    }

    /// Refuses a chat, and a workspace that cannot become one now: one the
    /// user holds, another builds on, or whose unsaved files no checkpoint
    /// can keep.
    async fn convertible_workspace(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Workspace> {
        let workspace = self.workspace(selector).await?;
        self.ensure_not_converting(&workspace.id, &workspace.name)
            .await?;
        target_checks::ready(&workspace, workspace.state).require()?;
        ensure!(
            !(workspace.holds.iter()).any(|hold| hold.placed_by == HoldPlacer::User),
            "the user holds workspace {}; release the hold first",
            workspace.name
        );
        if let Some(stacked) = workspace.stacked_workspaces.first() {
            bail!(
                "workspace {} builds on the branch of {}; it keeps its worktree while another builds on it",
                stacked.name,
                workspace.name
            );
        }
        let selector = SpaceSelector::id(&workspace.id);
        let inspection = (self.check_removal(daemon, &selector, InspectionPolicy::GitOnly)).await?;
        ensure!(
            !inspection.check.dirty
                || (self.checkpoint_keeps_unsaved_files(
                    daemon,
                    &workspace,
                    CheckpointCause::Removal
                ))
                .await?,
            "a checkpoint cannot keep the unsaved files of {}; commit or delete them first",
            workspace.name
        );
        Ok(workspace)
    }
}

#[cfg(test)]
mod tests;
