//! GitLab's push access, and the forks it opens no merge requests from.
use anyhow::Result;

use crate::{
    capability,
    repo::Repositories,
    transport::{
        account::Account,
        api::{Request, TransportRequests, encode},
        backend::LoginHint,
    },
};
use shoal_forge_types::{ForgeRepo, backends::gitlab::GitLab, capability::Capability};

impl Repositories for GitLab {
    /// Developers and above may push.
    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        let project = repo
            .send(
                account,
                &Request::get(format!("projects/{}", encode(&repo.path, false))),
            )
            .await
            .hint_login(self)?;
        let access = ["project_access", "group_access"]
            .iter()
            .filter_map(|scope| project["permissions"][scope]["access_level"].as_u64())
            .max();
        Ok(access.is_some_and(|level| level >= 30))
    }

    async fn fork(&self, _repo: &ForgeRepo, _account: &Account) -> Result<String> {
        Err(capability::unsupported(self, Capability::Forks))
    }
}
