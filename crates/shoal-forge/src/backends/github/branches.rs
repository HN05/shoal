//! GitHub's branches. Only its GraphQL API lists them with their commits'
//! times, and only to a login, so an anonymous reader lists them through
//! REST without those times.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{GitHub, graphql::graphql_nodes, rulesets::DeletionRules};
use crate::{
    backends::github_rest,
    branches::{BranchFields, Branches},
    transport::{account::Account, api::TransportRequests},
};
use shoal_forge_types::{
    ForgeRepo,
    branches::{ForgeBranch, OpenPull},
};

const BRANCHES: &str = "query($owner: String!, $name: String!, $after: String) { repository(owner: $owner, name: $name) { refs(refPrefix: \"refs/heads/\", first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { name target { oid ... on Commit { committedDate } } branchProtectionRule { id } } } } }";

const GRAPHQL_FIELDS: BranchFields = BranchFields {
    name: "/name",
    head: "/target/oid",
    committed_at: Some("/target/committedDate"),
};

const REST_FIELDS: BranchFields = BranchFields {
    name: "/name",
    head: "/commit/sha",
    committed_at: None,
};

impl Branches for GitHub {
    async fn branches(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<ForgeBranch>> {
        let mut branches = match account.reads_anonymously(repo) {
            true => rest_branches(repo, account).await?,
            false => graphql_branches(repo, account).await?,
        };
        let rules = DeletionRules::read(repo, account).await?;
        for branch in &mut branches {
            branch.protected |= rules.protects(branch.name.as_str());
        }
        Ok(branches)
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
}

/// The repository's branches with their commits' times, protected where a
/// branch protection rule covers them.
async fn graphql_branches(repo: &ForgeRepo, account: &Account) -> Result<Vec<ForgeBranch>> {
    let (owner, name) =
        (repo.path.split_once('/')).context("GitHub repository path has no owner")?;
    let variables = json!({ "owner": owner, "name": name });
    let nodes = graphql_nodes(repo, account, BRANCHES, variables, |data| {
        &data["repository"]["refs"]
    })
    .await?;
    (nodes.iter())
        .map(|branch| GRAPHQL_FIELDS.read(branch, branch["branchProtectionRule"].is_object()))
        .collect()
}

/// The repository's branches as REST lists them to anyone, without their
/// commits' times.
async fn rest_branches(repo: &ForgeRepo, account: &Account) -> Result<Vec<ForgeBranch>> {
    let listed = repo.pages(account, &repo.repo_endpoint("branches")).await?;
    (listed.iter())
        .map(|branch| REST_FIELDS.read(branch, branch["protected"] == true))
        .collect()
}
