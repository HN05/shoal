//! Forgejo's branches, which it lists with their protection.
use anyhow::Result;
use serde_json::Value;

use super::Forgejo;
use crate::{
    backends::{BranchFields, Branches, github_rest},
    transport::api::{Request, TransportRequests, encode},
};
use shoal_forge_types::{
    ForgeRepo,
    branches::{ForgeBranch, OpenPull},
    raw::HttpMethod,
};

const FIELDS: BranchFields = BranchFields {
    name: "/name",
    head: "/commit/id",
    committed_at: Some("/commit/timestamp"),
};

impl Branches for Forgejo {
    async fn branches(
        &self,
        repo: &ForgeRepo,
        account: &crate::account::Account,
    ) -> Result<Vec<ForgeBranch>> {
        let listed = repo.pages(account, &repo.repo_endpoint("branches")).await?;
        (listed.iter())
            .map(|branch| FIELDS.read(branch, branch["protected"] == true))
            .collect()
    }

    fn open_pulls_path(&self) -> &'static str {
        "pulls?state=open"
    }

    fn open_pull(&self, repo: &ForgeRepo, value: &Value) -> Result<OpenPull> {
        github_rest::open_pull(repo, value)
    }

    async fn outgoing_pulls(
        &self,
        repo: &ForgeRepo,
        account: &crate::account::Account,
    ) -> Result<Vec<OpenPull>> {
        github_rest::outgoing_pulls(repo, account).await
    }

    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request {
        let endpoint = repo.repo_endpoint(&format!("branches/{}", encode(branch, false)));
        Request::new(HttpMethod::Delete, endpoint, None)
    }
}
