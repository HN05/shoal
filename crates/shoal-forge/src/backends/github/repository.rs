use anyhow::Result;

use crate::{backends::github_rest, repo::Repositories, transport::account::Account};
use shoal_forge_types::{ForgeRepo, backends::github::GitHub};

impl Repositories for GitHub {
    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        github_rest::pushable(repo, account).await
    }

    /// GitHub answers with the account's existing fork when it has one.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String> {
        github_rest::create_fork(repo, account).await
    }
}
