//! The forge item actions go to, and the account a place changes items as.
use anyhow::Result;
use shoal_forge::transport::account::Account;
use shoal_forge_types::ForgeRepo;
use shoal_protocol::model::{Repository, Workspace};

use crate::{Links, daemon::Daemon, forges};

impl Links {
    /// The forge of the workspace's `origin`, with the agent account.
    pub async fn workspace_forge_account(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<(ForgeRepo, Account)> {
        let forge = forges::origin(daemon, &workspace.path).await?;
        Ok((forge, self.agent_account()))
    }

    /// The forge of the registered checkout's `origin`, with the agent
    /// account.
    pub async fn checkout_forge_account(
        &self,
        daemon: &dyn Daemon,
        repository: &Repository,
    ) -> Result<(ForgeRepo, Account)> {
        let forge = forges::of_checkout(daemon, repository).await?;
        Ok((forge, self.agent_account()))
    }
}
