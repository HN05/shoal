//! Starting a workspace's containers with its setup and its agent, where
//! its repository sets `start = "setup"`.
use anyhow::{Result, anyhow};

use super::{ContainerAcquisition, Starter};
use crate::{
    Leases, callers::Caller, config::repo::ContainerStart, daemon::Daemon, model::Workspace,
};
use shoal_protocol::{acquirers::Acquirer, containers::ContainerState, space_names::SpaceSelector};

impl Leases {
    /// Start the containers of `selector`'s workspace after its setup
    /// command unless they run, which leaves them to whoever holds them: for
    /// its own agent when `for_agent` or an agent launch waits there, as one
    /// does for a retried setup, else for `caller`, who created it. A
    /// failure fails the workspace's setup.
    pub async fn set_up_containers(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        for_agent: bool,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let workspace = daemon.workspace(selector).await?;
        if !self.starts_with_setup(daemon, &workspace).await?
            || self.containers_run(&workspace).await?
        {
            return Ok(());
        }
        let for_agent = for_agent || daemon.has_queued_launch(&workspace.id).await?;
        // The agent takes the project over when it starts.
        let acquirer = match for_agent {
            true => Acquirer::User,
            false => self.acquirer(daemon, caller).await,
        };
        let starter = Starter {
            acquirer,
            for_agent,
            agent_start: false,
            reason: None,
        };
        let started = self
            .start_containers(daemon, &workspace, starter, caller)
            .await;
        if let Err(error) = started.and_then(started_or_refused) {
            self.stop_failed_containers(daemon, &workspace.id).await;
            let message = format!("containers did not start: {error:#}; retry with shoal setup");
            daemon.fail_setup(&workspace.id, message).await?;
            return Err(error);
        }
        Ok(())
    }

    /// Start the containers of `selector`'s workspace for the agent
    /// `caller` runs, as it starts: one set up for the workspace's agent, or
    /// held by an earlier agent whose release may still be pending, becomes
    /// its own, and one that does not run starts as its own.
    pub async fn start_agent_containers(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let workspace = daemon.workspace(selector).await?;
        if !self.starts_with_setup(daemon, &workspace).await? {
            return Ok(());
        }
        let starter = Starter {
            acquirer: self.acquirer(daemon, caller).await,
            for_agent: false,
            agent_start: true,
            reason: None,
        };
        let started = self
            .start_containers(daemon, &workspace, starter, caller)
            .await;
        started.and_then(started_or_refused)
    }

    /// Whether the project of `workspace` runs.
    async fn containers_run(&self, workspace: &Workspace) -> Result<bool> {
        let recorded = self.container_project(&workspace.id).await?;
        Ok(recorded.is_some_and(|recorded| recorded.project.state == ContainerState::Running))
    }

    /// Whether the repository of `workspace` starts its containers with
    /// setup.
    async fn starts_with_setup(&self, daemon: &dyn Daemon, workspace: &Workspace) -> Result<bool> {
        let settings = self.settings(daemon, workspace).await?.into_settings();
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
