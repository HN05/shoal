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
        let Some(listed) = listed_rulesets(repo, account).await? else {
            return Ok(Self::default());
        };
        let mut conditions = Vec::new();
        let active = (listed.iter())
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

/// How many rulesets a page lists.
const PAGE: usize = 100;

/// Every ruleset of `repo` and its owner, page by page; none where the
/// server has no rulesets.
async fn listed_rulesets(repo: &ForgeRepo, account: &Account) -> Result<Option<Vec<Value>>> {
    let mut listed = Vec::new();
    for page in 1.. {
        let path = format!("rulesets?includes_parents=true&per_page={PAGE}&page={page}");
        let read = Request::get(repo.repo_endpoint(&path));
        let Ok(response) = repo.read_refusable(account, &read, &[404]).await? else {
            return Ok(None);
        };
        let rulesets = (response.as_array()).context("GitHub did not list rulesets")?;
        listed.extend(rulesets.iter().cloned());
        if rulesets.len() < PAGE {
            break;
        }
    }
    Ok(Some(listed))
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

/// Whether `pattern` matches `text` as GitHub matches ref names: `*`, `?`
/// and `[...]` within one path component, `**` across them, and `**/` also
/// across none.
fn fnmatch(pattern: &[u8], text: &[u8]) -> bool {
    match pattern {
        [] => text.is_empty(),
        [b'*', b'*', b'/', rest @ ..] => {
            fnmatch(rest, text)
                || (0..text.len()).any(|at| text[at] == b'/' && fnmatch(rest, &text[at + 1..]))
        }
        [b'*', b'*', rest @ ..] => (0..=text.len()).any(|at| fnmatch(rest, &text[at..])),
        [b'*', rest @ ..] => {
            let run = text.iter().take_while(|&&byte| byte != b'/').count();
            (0..=run).any(|at| fnmatch(rest, &text[at..]))
        }
        [b'?', rest @ ..] => {
            text.first().is_some_and(|&byte| byte != b'/') && fnmatch(rest, &text[1..])
        }
        [b'[', class @ ..] => match (class_matches(class, text.first().copied()), text) {
            (Some((true, rest)), [_, after @ ..]) => fnmatch(rest, after),
            (Some(_), _) => false,
            // An unclosed bracket is a literal one.
            (None, _) => text.first() == Some(&b'[') && fnmatch(class, &text[1..]),
        },
        [literal, rest @ ..] => text.first() == Some(literal) && fnmatch(rest, &text[1..]),
    }
}

/// Whether the character class that `class` opens, after its `[`, holds
/// `byte`, with the pattern after its `]`; none when it never closes. `!`
/// or `^` first negates it, and `a-z` is a range; no class holds `/`.
fn class_matches(class: &[u8], byte: Option<u8>) -> Option<(bool, &[u8])> {
    let (negated, mut at) = match class.first() {
        Some(b'!' | b'^') => (true, 1),
        _ => (false, 0),
    };
    let mut held = false;
    let mut first = true;
    while at < class.len() {
        match &class[at..] {
            [b']', ..] if !first => {
                let matches = byte.is_some_and(|byte| byte != b'/' && held != negated);
                return Some((matches, &class[at + 1..]));
            }
            [low, b'-', high, ..] if *high != b']' => {
                held |= byte.is_some_and(|byte| (*low..=*high).contains(&byte));
                at += 3;
            }
            [single, ..] => {
                held |= byte == Some(*single);
                at += 1;
            }
            [] => break,
        }
        first = false;
    }
    None
}

#[cfg(test)]
mod tests;
