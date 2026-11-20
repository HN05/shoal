//! Setups, as `shoal repo setup` and `shoal config setup` launch them: the
//! launches whose agent starts in the setup mode, which administers a
//! repository's config or the global config.
use anyhow::{Context as _, Result};
use shoal_daemon_base::permitted_actions::Action;
use shoal_protocol::launches::{Launch, Setup};

use crate::{
    config::permissions::PermissionKey,
    daemon::{manager::Manager, scope::Caller},
    launches,
};

impl Manager {
    /// Let the scoped `caller` start the setup `launch` asks for as its
    /// `admin` permission allows, in the repository it sets up or without
    /// one for the global config; any other launch, and the user's, needs
    /// nothing here.
    pub(super) async fn authorize_setup(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let (Some(setup), Some(caller)) = (launch.setup, caller) else {
            return Ok(());
        };
        match setup {
            Setup::Repository => {
                let action = self.repository_setup(launch, caller).await?;
                self.authorize_action(Some(caller), action).await
            }
            Setup::Config => self.authorize_config_setup(caller).await,
        }
    }

    /// Let the scoped `caller` start the global config's setup as its
    /// `admin` permission allows without a repository, whether a launch
    /// starts it or the caller queues the agent it runs itself.
    pub(crate) async fn authorize_config_setup(&self, caller: &Caller) -> Result<()> {
        let action = Action::new(PermissionKey::Admin, "shoal config setup");
        self.authorize_action(Some(caller), action).await
    }

    /// The action of the repository setup `launch` asks for, in the
    /// repository it names.
    async fn repository_setup(&self, launch: &Launch, caller: &Caller) -> Result<Action> {
        let state = self.launch_state(launch, Some(caller)).await?;
        let repo = (launches::shape::launch_repository(&state, launch).await?)
            .context("a repository's setup names its repository")?;
        let action = Action::new(
            PermissionKey::Admin,
            format!("shoal repo setup {}", repo.id),
        );
        Ok(action.in_repository(repo.id))
    }
}

#[cfg(test)]
mod tests;
