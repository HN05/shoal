//! GitHub's rulesets that forbid deleting branches, which protect a branch
//! as a classic branch protection rule does.
use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    account::Account,
    transport::api::{Request, TransportRequests},
};
use shoal_forge_types::ForgeRepo;

/// The branches the active rulesets of a repository and its owner keep from
/// deletion.
#[derive(Default)]
pub(super) struct DeletionRules {
    /// Each ruleset's ref name condition.
    conditions: Vec<RefNames>,
    /// The repository's default branch, which `~DEFAULT_BRANCH` names.
    default: String,
}

/// A ruleset's ref name condition: the refs it includes, less those it
/// excludes, as patterns or `~ALL` and `~DEFAULT_BRANCH`.
#[derive(Debug, PartialEq, Eq)]
struct RefNames {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl DeletionRules {
    /// The rules of `repo`'s rulesets and its owner's; none where the
    /// server has no rulesets.
    pub(super) async fn read(repo: &ForgeRepo, account: &Account) -> Result<Self> {
        let list = "rulesets?includes_parents=true&per_page=100";
        let list = Request::get(repo.repo_endpoint(list));
        let Ok(listed) = repo.read_refusable(account, &list, &[404]).await? else {
            return Ok(Self::default());
        };
        let mut conditions = Vec::new();
        let active = (listed.as_array().into_iter().flatten())
            .filter(|ruleset| ruleset["target"] == "branch" && ruleset["enforcement"] == "active");
        for ruleset in active {
            let id = (ruleset["id"].as_u64()).context("GitHub listed a ruleset without its ID")?;
            let path = format!("rulesets/{id}?includes_parents=true");
            let detail = repo
                .send(account, &Request::get(repo.repo_endpoint(&path)))
                .await?;
            conditions.extend(deleting_ref_names(&detail));
        }
        if conditions.is_empty() {
            return Ok(Self::default());
        }
        let repository = repo.read_repository(account).await?;
        let default = repository["default_branch"].as_str().unwrap_or_default();
        Ok(Self {
            conditions,
            default: default.to_owned(),
        })
    }

    /// Whether a ruleset keeps `branch` from deletion.
    pub(super) fn protects(&self, branch: &str) -> bool {
        let reference = format!("refs/heads/{branch}");
        let named = |patterns: &[String]| {
            (patterns.iter()).any(|pattern| self.names(pattern, branch, &reference))
        };
        (self.conditions.iter())
            .any(|condition| named(&condition.include) && !named(&condition.exclude))
    }

    /// Whether the ref name `pattern` names `branch`, whose full ref is
    /// `reference`.
    fn names(&self, pattern: &str, branch: &str, reference: &str) -> bool {
        match pattern {
            "~ALL" => true,
            "~DEFAULT_BRANCH" => branch == self.default,
            pattern => fnmatch(pattern.as_bytes(), reference.as_bytes()),
        }
    }
}

/// The ref name condition of `ruleset` when one of its rules forbids
/// deletion.
fn deleting_ref_names(ruleset: &Value) -> Option<RefNames> {
    let rules = ruleset["rules"].as_array()?;
    if !rules.iter().any(|rule| rule["type"] == "deletion") {
        return None;
    }
    let names = &ruleset["conditions"]["ref_name"];
    let patterns = |field: &str| -> Vec<String> {
        (names[field].as_array().into_iter().flatten())
            .filter_map(|pattern| pattern.as_str().map(str::to_owned))
            .collect()
    };
    Some(RefNames {
        include: patterns("include"),
        exclude: patterns("exclude"),
    })
}

/// Whether `pattern` matches `text` as GitHub matches ref names: `*` and
/// `?` within one path component, `**` across them.
fn fnmatch(pattern: &[u8], text: &[u8]) -> bool {
    match pattern {
        [] => text.is_empty(),
        [b'*', b'*', rest @ ..] => (0..=text.len()).any(|at| fnmatch(rest, &text[at..])),
        [b'*', rest @ ..] => {
            let run = text.iter().take_while(|&&byte| byte != b'/').count();
            (0..=run).any(|at| fnmatch(rest, &text[at..]))
        }
        [b'?', rest @ ..] => {
            text.first().is_some_and(|&byte| byte != b'/') && fnmatch(rest, &text[1..])
        }
        [literal, rest @ ..] => text.first() == Some(literal) && fnmatch(rest, &text[1..]),
    }
}

#[cfg(test)]
mod tests;
