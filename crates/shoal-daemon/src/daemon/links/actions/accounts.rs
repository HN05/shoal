//! The forge item actions go to, and the account a place changes items as.
use anyhow::Result;

use crate::{
    daemon::{links::place::Place, manager::Manager},
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

    /// The account that changes items from `place`: the agent account in a
    /// space, and the user's own logins in a registered checkout, where only
    /// the user acts.
    pub(in crate::daemon::links) fn place_account(&self, place: &Place) -> Account {
        match place {
            Place::Checkout(_) => self.links.user_account(),
            Place::Space(_) => self.links.agent_account(),
        }
    }
}
