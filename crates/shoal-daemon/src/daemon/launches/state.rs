//! What resolving a launch reads of the daemon's state, which
//! [`crate::launches`] decides over.
use anyhow::Result;
use shoal_protocol::launches::Launch;

use crate::{
    daemon::{manager::Manager, scope::Caller},
    launches::{LaunchState, SpaceRepository},
    model::Workspace,
};

impl Manager {
    /// What resolving `launch` reads for `caller`: the registered
    /// repositories, the workspaces it sees and the repository of the space
    /// it goes below.
    pub(super) async fn launch_state(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<LaunchState> {
        let parent = match &launch.parent {
            Some(parent) => self.parent_repository(parent).await?,
            None => SpaceRepository::Without,
        };
        Ok(LaunchState {
            repositories: self.repositories().await?,
            workspaces: self.launch_workspaces(caller).await?,
            parent,
        })
    }

    /// The workspaces `caller` sees.
    pub(super) async fn launch_workspaces(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<Workspace>> {
        let spaces = self.visible_spaces(caller).await?;
        Ok((spaces.into_iter())
            .filter_map(|space| space.into_workspace().ok())
            .collect())
    }

    /// Refuse what opening the existing branch `existing` in the repository
    /// `repository` names would refuse, such as a local branch of its name
    /// that does not track it.
    pub(super) async fn check_opens(
        &self,
        repository: &crate::repository::Selector,
        existing: &str,
    ) -> Result<()> {
        let options = crate::daemon::workspace::NewWorktreeOptions::default();
        (self.check_creation(repository, None, Some(existing), options)).await
    }
}
