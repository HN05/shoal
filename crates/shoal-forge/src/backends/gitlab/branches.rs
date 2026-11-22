//! GitLab's branches, which it lists with their protection, and its merge
//! requests by the branches they join.
use anyhow::{Context, Result};
use serde_json::Value;

use super::GitLab;
use crate::{
    backends::{BranchFields, Branches},
    transport::{account::Account, api::TransportRequests},
};
use shoal_forge_types::{
    ForgeRepo,
    branches::{ForgeBranch, OpenPull},
    numbers::PullNumber,
};

const FIELDS: BranchFields = BranchFields {
    name: "/name",
    head: "/commit/id",
    committed_at: Some("/commit/committed_date"),
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
            base: Some(branch("target_branch")?.into()),
        })
    }

    /// GitLab lists a merge request under the project it merges into.
    async fn outgoing_pulls(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<OpenPull>> {
        let read = repo.read_repository(account).await?;
        let parent = &read["forked_from_project"];
        let Some(path) = parent["path_with_namespace"].as_str() else {
            return Ok(Vec::new());
        };
        let parent = ForgeRepo::on(repo.kind(), repo.host.clone(), path, repo.web_scheme())?;
        let endpoint = parent.repo_endpoint(self.open_pulls_path());
        let listed = parent.pages(account, &endpoint).await?;
        let mut outgoing = Vec::new();
        for request in listed.iter() {
            if request["source_project_id"] == read["id"] {
                let pull = self.open_pull(&parent, request)?;
                outgoing.push(OpenPull {
                    fork: false,
                    base: None,
                    ..pull
                });
            }
        }
        Ok(outgoing)
    }
}
