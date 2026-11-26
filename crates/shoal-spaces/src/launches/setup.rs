//! Setups, as `shoal repo setup` and `shoal config setup` launch them:
//! launches whose agent starts in the setup mode.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result};
use shoal_daemon_base::permitted_actions::Action;
use shoal_protocol::launches::{Launch, Setup};

use crate::{config::permissions::PermissionKey, scope::Caller};
use shoal_core::launches;

impl Spaces {
    /// A scoped `caller` starts a setup as its `admin` permission allows, in
    /// the repository it sets up, or without one for the global config. Any
    /// other launch, and the user's, needs nothing here.
    pub(super) async fn authorize_setup(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let (Some(setup), Some(caller)) = (launch.setup, caller) else {
            return Ok(());
        };
        match setup {
            Setup::Repository => {
                let action = self.repository_setup(daemon, launch, caller).await?;
                daemon
                    .coordination()
                    .authorize_action(daemon, Some(caller), action)
                    .await
            }
            Setup::Config => self.authorize_config_setup(daemon, caller).await,
        }
    }

    /// Whether a launch starts it or the caller queues the agent it runs
    /// itself.
    pub async fn authorize_config_setup(&self, daemon: &dyn Daemon, caller: &Caller) -> Result<()> {
        let action = Action::new(PermissionKey::Admin, "shoal config setup");
        daemon
            .coordination()
            .authorize_action(daemon, Some(caller), action)
            .await
    }

    async fn repository_setup(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: &Caller,
    ) -> Result<Action> {
        let state = self.launch_state(daemon, launch, Some(caller)).await?;
        let repo = (launches::shape::launch_repository(&state, launch).await?)
            .context("a repository's setup names its repository")?;
        let action = Action::new(
            PermissionKey::Admin,
            format!("shoal repo setup {}", repo.id),
        );
        Ok(action.in_repository(repo.id))
    }
}
