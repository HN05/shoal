//! GitHub's branches, which only its GraphQL API lists with their commits'
//! times, protected by a branch protection rule or a ruleset, and its PRs
//! by the branches they join.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{GitHub, graphql::graphql_nodes, rulesets::DeletionRules};
use crate::{
    account::Account,
    backends::{BranchFields, Branches, github_rest},
    transport::api::{Request, TransportRequests, encode},
};
use shoal_forge_types::{
    ForgeRepo,
    branches::{ForgeBranch, OpenPull},
    raw::HttpMethod,
};

const BRANCHES: &str = "query($owner: String!, $name: String!, $after: String) { repository(owner: $owner, name: $name) { refs(refPrefix: \"refs/heads/\", first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { name target { oid ... on Commit { committedDate } } branchProtectionRule { id } } } } }";

const FIELDS: BranchFields = BranchFields {
    name: "/name",
    head: "/target/oid",
    committed_at: "/target/committedDate",
};

impl Branches for GitHub {
    async fn branches(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<ForgeBranch>> {
        let (owner, name) =
            (repo.path.split_once('/')).context("GitHub repository path has no owner")?;
        let variables = json!({ "owner": owner, "name": name });
        let nodes = graphql_nodes(repo, account, BRANCHES, variables, |data| {
            &data["repository"]["refs"]
        })
        .await?;
        let rules = DeletionRules::read(repo, account).await?;
        (nodes.iter())
            .map(|branch| {
                let name = branch["name"].as_str().unwrap_or_default();
                let protected = branch["branchProtectionRule"].is_object() || rules.protects(name);
                FIELDS.read(branch, protected)
            })
            .collect()
    }

    fn open_pulls_path(&self) -> &'static str {
        "pulls?state=open"
    }

    fn open_pull(&self, repo: &ForgeRepo, value: &Value) -> Result<OpenPull> {
        github_rest::open_pull(repo, value)
    }

    async fn outgoing_pulls(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<OpenPull>> {
        github_rest::outgoing_pulls(repo, account).await
    }

    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request {
        let endpoint = repo.repo_endpoint(&format!("git/refs/heads/{}", encode(branch, true)));
        Request::new(HttpMethod::Delete, endpoint, None)
    }
}
