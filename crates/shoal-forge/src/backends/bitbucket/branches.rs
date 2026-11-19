//! Bitbucket's branches, which its branch restrictions protect from
//! deletion, and its PRs by the branches they join.
use anyhow::{Context, Result};
use serde_json::Value;

use super::Bitbucket;
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
    head: "/target/hash",
    committed_at: "/target/date",
};

impl Branches for Bitbucket {
    async fn branches(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<ForgeBranch>> {
        let listed = repo
            .pages(account, &repo.repo_endpoint("refs/branches"))
            .await?;
        let restrictions = "branch-restrictions?kind=delete";
        let restricted = repo
            .pages(account, &repo.repo_endpoint(restrictions))
            .await?;
        restricted_branches(&listed, &restricted)
    }

    fn open_pulls_path(&self) -> &'static str {
        "pullrequests?state=OPEN"
    }

    fn open_pull(&self, repo: &ForgeRepo, value: &Value) -> Result<OpenPull> {
        let number = (value["id"].as_u64()).context("Bitbucket listed a PR without its number")?;
        let branch = |side: &str| {
            value[side]["branch"]["name"]
                .as_str()
                .with_context(|| format!("Bitbucket listed PR #{number} without its {side} branch"))
        };
        let head_repository = value["source"]["repository"]["full_name"].as_str();
        Ok(OpenPull {
            number: PullNumber::new(number),
            head: branch("source")?.into(),
            fork: !head_repository.is_some_and(|name| name.eq_ignore_ascii_case(&repo.path)),
            base: branch("destination")?.into(),
        })
    }

    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request {
        let endpoint = repo.repo_endpoint(&format!("refs/branches/{}", encode(branch, false)));
        Request::new(HttpMethod::Delete, endpoint, None)
    }
}

/// The branches `listed`, each protected where a restriction of
/// `restricted` that names its branches by a pattern matches it.
fn restricted_branches(listed: &[Value], restricted: &[Value]) -> Result<Vec<ForgeBranch>> {
    let patterns: Vec<&str> = (restricted.iter())
        .filter(|restriction| restriction["branch_match_kind"] == "glob")
        .filter_map(|restriction| restriction["pattern"].as_str())
        .collect();
    (listed.iter())
        .map(|branch| {
            let name = branch["name"].as_str().unwrap_or_default();
            let protected = patterns.iter().any(|pattern| glob_matches(pattern, name));
            FIELDS.read(branch, protected)
        })
        .collect()
}

/// Whether the branch restriction pattern `pattern`, where `*` stands for
/// any text, matches `name`.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[cfg(test)]
mod tests;
