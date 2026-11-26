use anyhow::Result;

use crate::{
    backends::github_rest,
    repo::Repositories,
    transport::{account::Account, api::TransportRequests},
};
use shoal_forge_types::{ForgeRepo, backends::forgejo::Forgejo};

impl Repositories for Forgejo {
    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        github_rest::pushable(repo, account).await
    }

    /// Forgejo refuses a second fork, so an existing one is looked up first.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String> {
        match owned_fork(repo, account).await? {
            Some(fork) => Ok(fork),
            None => github_rest::create_fork(repo, account).await,
        }
    }
}

/// `owner/repository` of the account's existing fork of the repository.
async fn owned_fork(repo: &ForgeRepo, account: &Account) -> Result<Option<String>> {
    let login = github_rest::own_login(repo, account).await?;
    let forks = repo.pages(account, &repo.repo_endpoint("forks")).await?;
    forks
        .iter()
        .find(|fork| {
            fork["owner"]["login"]
                .as_str()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(&login))
        })
        .map(github_rest::full_name)
        .transpose()
}
