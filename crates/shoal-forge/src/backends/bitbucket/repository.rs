use anyhow::Result;
use serde_json::Value;

use super::literal;
use crate::{
    capability,
    repo::Repositories,
    transport::{
        account::Account,
        api::{TransportRequests, encode},
    },
};
use shoal_forge_types::{ForgeRepo, backends::bitbucket::Bitbucket, capability::Capability};

impl Repositories for Bitbucket {
    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        let query = encode(
            &format!("repository.full_name={}", literal(&repo.path)),
            false,
        );
        let endpoint = format!("user/permissions/repositories?q={query}");
        let grants = repo.pages(account, &endpoint).await?;
        Ok(grants
            .iter()
            .any(|grant| matches!(grant["permission"].as_str(), Some("write" | "admin"))))
    }

    async fn fork(&self, _repo: &ForgeRepo, _account: &Account) -> Result<String> {
        Err(capability::unsupported(self, Capability::Forks))
    }

    fn default_branch<'a>(&self, value: &'a Value) -> Option<&'a str> {
        value["mainbranch"]["name"].as_str()
    }
}
