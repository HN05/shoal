//! The base another clone of a repository, from the same `origin`, starts a
//! branch from for a revision this repository names: one it can fetch from
//! that remote.
use std::path::Path;

use anyhow::Result;

use super::{resolve_commit, resolve_named_commit, run_isolated};

/// The remote another clone fetches from, as a URL clone names it.
const ORIGIN: &str = "origin";

/// The base another clone of `repo` from its `origin` starts from for
/// `rev`: `origin/<branch>` where `rev` names a branch whose commit the
/// origin's branch of that name has at its tip, else the full ID of its
/// commit where a branch of the origin holds that commit; none where only
/// this repository has it, as for unpushed commits.
pub async fn for_clone(repo: &Path, rev: &str) -> Result<Option<String>> {
    let (commit, name) = resolve_named_commit(repo, rev, run_isolated).await?;
    let remote = format!("refs/remotes/{ORIGIN}/");
    let branch = (name.strip_prefix("refs/heads/")).or(name.strip_prefix(remote.as_str()));
    if let Some(branch) = branch {
        let tracking = format!("{remote}{branch}");
        if resolve_commit(repo, &tracking, run_isolated).await.ok() == Some(commit.clone()) {
            return Ok(Some(format!("{ORIGIN}/{branch}")));
        }
    }
    let contains = ["for-each-ref", "--count=1", "--contains", commit.as_str()];
    let holding = run_isolated(repo, &[&contains[..], &[remote.as_str()]].concat()).await?;
    Ok((!holding.trim().is_empty()).then(|| commit.to_string()))
}

#[cfg(test)]
mod tests;
