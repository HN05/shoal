//! A workspace becoming a chat: the refusals, then the conversion its
//! agent's turn end carries out, which removes the worktree and restarts the
//! agent in the repository's checkout.
use anyhow::{Context as _, Result, bail, ensure};

use super::Pending;
use crate::client::space_names::SpaceSelector;
use crate::{
    config::modes::ModeName,
    daemon::{scope::Caller, workspace::Manager},
    model::{HoldPlacer, PresentSpace, Workspace},
    removal::InspectionPolicy,
    state::WorkspaceState,
};

impl Manager {
    /// Record that the workspace `selector` names becomes a chat in `mode`,
    /// for the space `caller` runs in, once its agent's turn ends; the
    /// checks of the removal it makes then come first.
    pub(crate) async fn convert_to_chat(
        &self,
        selector: &SpaceSelector,
        mode: ModeName,
        caller: Option<&Caller>,
    ) -> Result<Workspace> {
        let workspace = self.convertible_workspace(selector, caller).await?;
        let execution = self
            .running_agents()
            .await?
            .into_iter()
            .find(|agent| agent.space_id == workspace.id && agent.agent.is_some())
            .map(|agent| agent.execution)
            .with_context(|| {
                format!(
                    "no agent runs in {} to carry on in a chat; start one first",
                    workspace.name
                )
            })?;
        let (id, pending) = (workspace.id.clone(), Pending::Chat { mode, execution });
        self.store
            .write(move |tx| super::record::record(tx, &id, &pending))
            .await?;
        // The relocation waits for the agent's turn to end, which it may
        // have already.
        self.signals.agent_changed();
        Ok(workspace)
    }

    /// The workspace `selector` names, refusing a chat, and a workspace that
    /// cannot become one now: one the user holds, another builds on, whose
    /// unsaved files no checkpoint can keep, or another space's for
    /// `caller`.
    async fn convertible_workspace(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<Workspace> {
        let workspace = match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => workspace,
            PresentSpace::Chat(chat) => bail!("{} is a chat already", chat.name),
        };
        self.ensure_own_space(&workspace.id, &workspace.name, caller)?;
        self.ensure_not_converting(&workspace.id, &workspace.name)
            .await?;
        ensure!(
            workspace.state == WorkspaceState::Ready,
            "workspace {} is {}",
            workspace.name,
            workspace.state
        );
        ensure!(
            !(workspace.holds.iter()).any(|hold| hold.placed_by == HoldPlacer::User),
            "the user holds workspace {}; ask the user to release the hold first",
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
        let inspection = (self.check_removal(&selector, InspectionPolicy::GitOnly)).await?;
        ensure!(
            !inspection.check.dirty || self.archives_unsaved_files(&workspace).await?,
            "a checkpoint cannot keep the unsaved files of {}; commit or delete them first",
            workspace.name
        );
        Ok(workspace)
    }
}

#[cfg(test)]
mod tests;
