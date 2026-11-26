//! What resolving a launch reads of the daemon's state.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_protocol::launches::Launch;

use crate::{model::Workspace, scope::Caller};
use shoal_core::launches::{LaunchState, SpaceRepository};

impl Spaces {
    /// The registered repositories, the workspaces `caller` sees and the
    /// repository of the space the launch goes below.
    pub(super) async fn launch_state(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<LaunchState> {
        let parent = match &launch.parent {
            Some(parent) => self.parent_repository(daemon, parent).await?,
            None => SpaceRepository::Without,
        };
        Ok(LaunchState {
            repositories: daemon.repositories().await?,
            workspaces: self.launch_workspaces(daemon, caller).await?,
            parent,
        })
    }

    pub(super) async fn launch_workspaces(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
    ) -> Result<Vec<Workspace>> {
        let spaces = daemon.coordination().visible_spaces(daemon, caller).await?;
        Ok((spaces.into_iter())
            .filter_map(|space| space.into_workspace().ok())
            .collect())
    }

    /// Such as a local branch of its name that does not track it.
    pub(super) async fn check_opens(
        &self,
        daemon: &dyn Daemon,
        repository: &crate::repository::Selector,
        existing: &str,
    ) -> Result<()> {
        let options = crate::workspace::NewWorktreeOptions::default();
        (self.check_creation(daemon, repository, None, Some(existing), options)).await
    }
}
