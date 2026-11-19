//! Bitbucket's branches, which its branch restrictions protect from
//! deletion by a pattern or by their kind in the branching model, and its
//! PRs by the branches they join.
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
        let by_model = (restricted.iter())
            .any(|restriction| restriction["branch_match_kind"] == "branching_model");
        let model = match by_model {
            true => {
                let read = Request::get(repo.repo_endpoint("effective-branching-model"));
                repo.send(account, &read).await?
            }
            false => Value::Null,
        };
        restricted_branches(&listed, &restricted, &model)
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
            base: Some(branch("destination")?.into()),
        })
    }

    /// Bitbucket lists a PR under the repository it merges into.
    async fn outgoing_pulls(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<OpenPull>> {
        let read = repo.read_repository(account).await?;
        let Some(parent) = read["parent"]["full_name"].as_str() else {
            return Ok(Vec::new());
        };
        let parent = ForgeRepo::on(repo.kind(), repo.host.clone(), parent, repo.web_scheme())?;
        let endpoint = parent.repo_endpoint(self.open_pulls_path());
        let listed = parent.pages(account, &endpoint).await?;
        let mut outgoing = Vec::new();
        for pull in listed.iter() {
            let pull = self.open_pull(repo, pull)?;
            if !pull.fork {
                outgoing.push(OpenPull { base: None, ..pull });
            }
        }
        Ok(outgoing)
    }

    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request {
        let endpoint = repo.repo_endpoint(&format!("refs/branches/{}", encode(branch, false)));
        Request::new(HttpMethod::Delete, endpoint, None)
    }
}

/// The branches `listed`, each protected where a restriction of
/// `restricted` matches it, by a pattern or by its kind in the repository's
/// branching `model`.
fn restricted_branches(
    listed: &[Value],
    restricted: &[Value],
    model: &Value,
) -> Result<Vec<ForgeBranch>> {
    (listed.iter())
        .map(|branch| {
            let name = branch["name"].as_str().unwrap_or_default();
            let protected = (restricted.iter())
                .any(|restriction| restriction_matches(restriction, model, name));
            FIELDS.read(branch, protected)
        })
        .collect()
}

/// Whether `restriction` names the branch `name`, as `model` defines the
/// kinds of branch a restriction may name.
fn restriction_matches(restriction: &Value, model: &Value, name: &str) -> bool {
    match restriction["branch_match_kind"].as_str() {
        Some("glob") => {
            (restriction["pattern"].as_str()).is_some_and(|pattern| glob_matches(pattern, name))
        }
        Some("branching_model") => (restriction["branch_type"].as_str())
            .is_some_and(|kind| model_matches(model, kind, name)),
        _ => false,
    }
}

/// Whether `name` is a branch of `kind` in the branching `model`: its
/// development or production branch by name, any other kind by its prefix.
fn model_matches(model: &Value, kind: &str, name: &str) -> bool {
    match kind {
        "development" | "production" => {
            let branch = &model[kind];
            branch["branch"]["name"]
                .as_str()
                .or(branch["name"].as_str())
                == Some(name)
        }
        _ => (model["branch_types"].as_array().into_iter().flatten()).any(|branch_type| {
            branch_type["kind"] == kind
                && (branch_type["prefix"].as_str()).is_some_and(|prefix| name.starts_with(prefix))
        }),
    }
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
