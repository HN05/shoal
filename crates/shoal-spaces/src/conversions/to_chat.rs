//! A workspace becoming a chat: the refusals, then the conversion its
//! agent's turn end carries out, which removes the worktree and restarts the
//! agent in the repository's checkout.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, bail, ensure};

use super::record::{Carrying, ConvertedChat};
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
    /// Record that the workspace `selector` names becomes a chat in `mode`,
    /// once its agent's turn ends, its agent carrying on as `change` asks
    /// for `caller`; the checks of the removal it makes then come first.
    pub async fn convert_to_chat(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        mode: ModeName,
        change: AgentChange,
        caller: Option<&Caller>,
    ) -> Result<Workspace> {
        let workspace = self.convertible_workspace(daemon, selector).await?;
        // The chat runs in the checkout, under its settings, not the worktree's.
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
            .write(move |tx| super::record::record_chat_conversion(tx, &id, chat, carrying))
            .await?;
        // The relocation waits for the agent's turn to end, which it may
        // have already.
        self.signals.agent_changed();
        Ok(workspace)
    }

    /// The stopped chat `workspace` becomes in `mode`: its running agent,
    /// with the permissions `change` gives it, which carries on in it as
    /// `carrying` says, or else the agent it saved last, whose work an agent
    /// tool `carrying` names takes over.
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

    /// Refuse an agent of the workspace `selector` names while it converts:
    /// one that becomes a chat, whose agent carries on there, or a chat that
    /// became it, whose agent's turn end, which another agent's reports
    /// would hide, moves it there. The caller holds the agents' admissions.
    pub async fn ensure_takes_agents(&self, selector: &SpaceSelector) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        let id = workspace.id.clone();
        let pending = self.store.run(move |db| super::pending(db, &id)).await?;
        match pending {
            Some(super::Pending::Chat { .. }) => bail!(
                "workspace {} becomes a chat once its agent's turn ends; restore the agent in the chat then",
                workspace.name
            ),
            Some(super::Pending::Workspace { .. }) => bail!(
                "the agent of the chat that became workspace {} moves there once its turn ends; start another agent there then",
                workspace.name
            ),
            Some(super::Pending::Agent { taking, .. }) => bail!(
                "the work of workspace {} goes over to {} once its agent's turn ends; start another agent there then",
                workspace.name,
                taking.agent
            ),
            None => Ok(()),
        }
    }

    /// The stopped chat the workspace `id` becomes in `mode`, whose agent
    /// `execution` runs.
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

    /// The workspace `selector` names, refusing a chat, and a workspace that
    /// cannot become one now: one the user holds, another builds on, whose
    /// unsaved files no checkpoint can keep.
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
