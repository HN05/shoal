//! The base another clone of a repository, from the same `origin`, starts a
//! branch from for a revision this repository names: one it can fetch from
//! that remote.
use std::path::Path;

use anyhow::Result;

use super::{FETCH_SAFE_ARGS, resolve_commit, resolve_named_commit, run_isolated};

/// The remote another clone fetches from, as a URL clone names it.
const ORIGIN: &str = "origin";

/// A base as another clone of the origin starts from it once it fetched
/// the origin's branch that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneBase {
    /// How that clone names the base: `origin/<branch>`, or a commit's
    /// full ID.
    pub base: String,
    /// The origin's branch that holds it.
    pub branch: String,
}

/// The base another clone of `repo` from its `origin` starts from for
/// `rev`: `origin/<branch>` where `rev` names a branch whose commit the
/// origin's branch of that name has at its tip, else the full ID of its
/// commit where a branch of the origin holds that commit; none where only
/// this repository has it, as for unpushed commits.
pub async fn for_clone(repo: &Path, rev: &str) -> Result<Option<CloneBase>> {
    let (commit, name) = resolve_named_commit(repo, rev, run_isolated).await?;
    let remote = format!("refs/remotes/{ORIGIN}/");
    let branch = (name.strip_prefix("refs/heads/")).or(name.strip_prefix(remote.as_str()));
    if let Some(branch) = branch {
        let tracking = format!("{remote}{branch}");
        if resolve_commit(repo, &tracking, run_isolated).await.ok() == Some(commit.clone()) {
            let base = format!("{ORIGIN}/{branch}");
            let branch = branch.to_owned();
            return Ok(Some(CloneBase { base, branch }));
        }
    }
    let format = "--format=%(refname:strip=3)";
    let contains = ["for-each-ref", format, "--contains", commit.as_str()];
    let holding = run_isolated(repo, &[&contains[..], &[remote.as_str()]].concat()).await?;
    // The origin's HEAD names one of its branches, which a clone fetches
    // by that branch's name.
    let branch = (holding.lines()).find(|branch| !branch.is_empty() && *branch != "HEAD");
    Ok(branch.map(|branch| CloneBase {
        base: commit.to_string(),
        branch: branch.to_owned(),
    }))
}

/// Fetch the origin's `branch` into `repo`, whatever branches its
/// configured refspec fetches, as for a base [`for_clone`] found there.
pub async fn fetch_branch(repo: &Path, branch: &str) -> Result<()> {
    let refspec = format!("+refs/heads/{branch}:refs/remotes/{ORIGIN}/{branch}");
    let fetch = [FETCH_SAFE_ARGS, &["--", ORIGIN, refspec.as_str()]].concat();
    run_isolated(repo, &fetch).await.map(drop)
}

#[cfg(test)]
mod tests;
