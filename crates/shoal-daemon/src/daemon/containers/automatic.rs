//! Starting a workspace's containers without `shoal acquire containers`,
//! where its repository sets `start = "setup"`: with its setup, before it
//! is ready, and before its agent starts.
use anyhow::{Result, anyhow};

use super::{ContainerAcquisition, Starter};
use crate::{
    config::repo::ContainerStart,
    daemon::{scope::Caller, workspace::Manager},
    model::Workspace,
    state::WorkspaceState,
};
use shoal_protocol::{acquirers::Acquirer, space_names::SpaceSelector};

impl Manager {
    /// Start the containers of `selector`'s workspace after its setup
    /// command, for its own agent when `for_agent`, else for `caller`, who
    /// created it. A failure fails the workspace's setup.
    pub async fn set_up_containers(
        &self,
        selector: &SpaceSelector,
        for_agent: bool,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        if !self.starts_with_setup(&workspace).await? {
            return Ok(());
        }
        // The agent takes the project over when it starts.
        let acquirer = match for_agent {
            true => Acquirer::User,
            false => self.acquirer(caller).await,
        };
        let starter = Starter {
            acquirer,
            for_agent,
            reason: None,
        };
        let started = self.start_containers(&workspace, starter, caller).await;
        if let Err(error) = started.and_then(started_or_refused) {
            let message = format!("containers did not start: {error:#}; retry with shoal setup");
            (self.set_state(&workspace.id, WorkspaceState::Failed, Some(message))).await?;
            return Err(error);
        }
        Ok(())
    }

    /// Start the containers of `selector`'s workspace for the agent
    /// `caller` runs, as it starts: one set up for the workspace's agent
    /// becomes its own, and one that does not run starts as its own.
    pub async fn start_agent_containers(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        if !self.starts_with_setup(&workspace).await? {
            return Ok(());
        }
        let starter = Starter {
            acquirer: self.acquirer(caller).await,
            for_agent: false,
            reason: None,
        };
        let started = self.start_containers(&workspace, starter, caller).await;
        started.and_then(started_or_refused)
    }

    /// Whether the repository of `workspace` starts its containers with
    /// setup.
    async fn starts_with_setup(&self, workspace: &Workspace) -> Result<bool> {
        let settings = self.workspace_settings(workspace).await?.into_settings();
        let containers = settings.containers;
        Ok(containers.compose.is_some() && containers.start == ContainerStart::Setup)
    }
}

/// A start that is not granted at once, since a port it reserves waits for
/// approval or its preferred port is taken, as the error that says so.
fn started_or_refused(acquisition: ContainerAcquisition) -> Result<()> {
    match acquisition {
        ContainerAcquisition::Started(_) => Ok(()),
        ContainerAcquisition::Approval(request) => {
            Err(anyhow!("port {} waits for an approval", request.name))
        }
        ContainerAcquisition::Suggested(suggestion) => Err(anyhow!(
            "port {}: {} is in use; reserve {} with `shoal acquire port {} --port {}`",
            suggestion.name,
            suggestion.requested_port,
            suggestion.suggested_port,
            suggestion.name,
            suggestion.suggested_port
        )),
    }
}
