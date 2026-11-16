//! Starting a workspace's containers without `shoal acquire containers`,
//! where its repository sets `start = "setup"`: once setup readies the
//! workspace, before its post-setup hook and agent, and before an agent
//! starts there.
use anyhow::{Result, anyhow};

use super::{ContainerAcquisition, Starter};
use crate::{
    config::repo::ContainerStart,
    daemon::{scope::Caller, workspace::Manager},
    model::Workspace,
    state::WorkspaceState,
};
use shoal_protocol::{acquirers::Acquirer, containers::ContainerState, space_names::SpaceSelector};

impl Manager {
    /// Start the containers of `selector`'s workspace after its setup
    /// command unless they run, which leaves them to whoever holds them: for
    /// its own agent when `for_agent` or an agent launch waits there, as one
    /// does for a retried setup, else for `caller`, who created it. A
    /// failure fails the workspace's setup.
    pub async fn set_up_containers(
        &self,
        selector: &SpaceSelector,
        for_agent: bool,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        if !self.starts_with_setup(&workspace).await? || self.containers_run(&workspace).await? {
            return Ok(());
        }
        let for_agent = for_agent || self.has_queued_launch(&workspace.id).await?;
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

    /// Whether the project of `workspace` runs.
    async fn containers_run(&self, workspace: &Workspace) -> Result<bool> {
        let recorded = self.container_project(&workspace.id).await?;
        Ok(recorded.is_some_and(|recorded| recorded.project.state == ContainerState::Running))
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
