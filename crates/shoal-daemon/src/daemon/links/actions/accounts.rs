//! The forge item actions go to, and the account a place changes items as.
use anyhow::Result;

use crate::{
    daemon::manager::Manager,
    forge::ForgeRepo,
    model::{Repository, Workspace},
};
use shoal_forge::account::Account;

impl Manager {
    /// The forge of the workspace's `origin`, with the agent account.
    pub(crate) async fn workspace_forge_account(
        &self,
        workspace: &Workspace,
    ) -> Result<(ForgeRepo, Account)> {
        Ok((
            shoal_links::forges::origin(self, &workspace.path).await?,
            self.links.agent_account(),
        ))
    }

    /// The forge of the registered checkout's `origin`, with the agent
    /// account.
    pub(in crate::daemon::links) async fn checkout_forge_account(
        &self,
        repository: &Repository,
    ) -> Result<(ForgeRepo, Account)> {
        Ok((
            shoal_links::forges::of_checkout(self, repository).await?,
            self.links.agent_account(),
        ))
    }
}
