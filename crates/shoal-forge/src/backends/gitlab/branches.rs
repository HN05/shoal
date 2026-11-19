//! GitLab's branches, which it lists with their protection, and its merge
//! requests by the branches they join.
use anyhow::{Context, Result};
use serde_json::Value;

use super::GitLab;
use crate::{
    account::Account,
    backends::{BranchFields, Branches},
    transport::api::{Request, TransportRequests, encode},
};
use shoal_forge_types::{
    ForgeRepo,
    branches::{ForgeBranch, OpenPull},
    numbers::PullNumber,
    raw::HttpMethod,
};

const FIELDS: BranchFields = BranchFields {
    name: "/name",
    head: "/commit/id",
    committed_at: "/commit/committed_date",
};

impl Branches for GitLab {
    async fn branches(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<ForgeBranch>> {
        let endpoint = repo.repo_endpoint("repository/branches");
        let listed = repo.pages(account, &endpoint).await?;
        (listed.iter())
            .map(|branch| FIELDS.read(branch, branch["protected"] == true))
            .collect()
    }

    fn open_pulls_path(&self) -> &'static str {
        "merge_requests?state=opened"
    }

    /// A merge request from a fork comes from another project.
    fn open_pull(&self, _repo: &ForgeRepo, value: &Value) -> Result<OpenPull> {
        let text = |field: &str| value[field].as_str();
        let number =
            (value["iid"].as_u64()).context("GitLab listed a merge request without its number")?;
        let branch = |field| {
            text(field).with_context(|| {
                format!("GitLab listed merge request !{number} without its {field}")
            })
        };
        Ok(OpenPull {
            number: PullNumber::new(number),
            head: branch("source_branch")?.into(),
            fork: value["source_project_id"] != value["target_project_id"],
            base: branch("target_branch")?.into(),
        })
    }

    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request {
        let endpoint =
            repo.repo_endpoint(&format!("repository/branches/{}", encode(branch, false)));
        Request::new(HttpMethod::Delete, endpoint, None)
    }
}
