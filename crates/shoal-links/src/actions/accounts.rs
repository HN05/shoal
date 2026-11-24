//! The forge item actions go to, and the account a place changes items as.
use anyhow::Result;
use shoal_forge::transport::account::Account;
use shoal_forge_types::ForgeRepo;
use shoal_protocol::model::{Repository, Workspace};

use crate::{Links, place::Place};

impl Links {
    /// The account that changes items from `place`: the agent account in a
    /// space, and the user's own logins in a registered checkout, where only
    /// the user acts.
    pub fn place_account(&self, place: &Place) -> Account {
        match place {
            Place::Checkout(_) => self.forges.user_account(),
            Place::Space(_) => self.forges.agent_account(),
        }
    }

    /// The forge of the workspace's `origin`, with the agent account.
    pub async fn workspace_forge_account(
        &self,
        workspace: &Workspace,
    ) -> Result<(ForgeRepo, Account)> {
        let forge = self.forges.origin_forge(&workspace.path).await?;
        Ok((forge, self.forges.agent_account()))
    }

    /// The forge of the registered checkout's `origin`, with the agent
    /// account.
    pub async fn checkout_forge_account(
        &self,
        repository: &Repository,
    ) -> Result<(ForgeRepo, Account)> {
        let forge = self.forges.checkout_forge(repository).await?;
        Ok((forge, self.forges.agent_account()))
    }
}
