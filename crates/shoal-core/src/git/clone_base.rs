//! The base another clone of a repository, from the same `origin`, starts a
//! branch from for a revision this repository names: one it can fetch from
//! that remote, or else a commit this repository carries there in a bundle
//! of the history the origin lacks.
use std::path::Path;

use anyhow::{Context as _, Result};
use shoal_refs::CommitId;

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

/// The commit `rev` names in `repo`, which a clone of the origin that lacks
/// it starts from once [`bundle_carried`] carried it there.
pub async fn carried_commit(repo: &Path, rev: &str) -> Result<CommitId> {
    resolve_commit(repo, rev, run_isolated).await
}

const CARRIED: &str = "refs/shoal/carried/";

fn carried_reference(commit: &CommitId) -> String {
    format!("{CARRIED}{commit}")
}

/// The commits [`fetch_carried`] took into `repo` that their carried refs
/// still keep.
pub async fn carried(repo: &Path) -> Result<Vec<CommitId>> {
    let refs = super::refs::list(repo, &[CARRIED]).await?;
    Ok((refs.into_iter())
        .map(|reference| reference.object.into())
        .collect())
}

/// Whether a branch of `repo` holds `commit`, which then stays reachable
/// without its carried ref; a checkpoint, which expires, does not.
pub async fn held_by_branch(repo: &Path, commit: &CommitId) -> Result<bool> {
    let format = "--format=%(refname)";
    let holding = [
        "for-each-ref",
        "--count=1",
        format,
        "--contains",
        commit.as_str(),
        "refs/heads/",
    ];
    Ok(!run_isolated(repo, &holding).await?.trim().is_empty())
}

/// Delete the carried ref of `commit` in `repo`, leaving the commit to
/// Git's collection unless another ref holds it.
pub async fn drop_carried(repo: &Path, commit: &CommitId) -> Result<()> {
    let reference = carried_reference(commit);
    run_isolated(repo, &["update-ref", "-d", &reference, commit.as_str()])
        .await
        .map(drop)
}

/// Write to `file` a bundle of `commit` with the history it has that the
/// origin's `branch`, as `repo` last fetched it, lacks, under a ref of its
/// own, which [`fetch_carried`] takes from it in another clone; bundles of
/// the same commit at once each have theirs.
pub async fn bundle_carried(
    repo: &Path,
    file: &Path,
    (commit, branch): (&CommitId, &str),
) -> Result<()> {
    let reference = format!("refs/shoal/carrying/{}", uuid::Uuid::new_v4());
    let file = file.to_str().context("bundle path is not UTF-8")?;
    run_isolated(repo, &["update-ref", &reference, commit.as_str()]).await?;
    let fetched = format!("refs/remotes/{ORIGIN}/{branch}");
    let bundle = [
        "bundle", "create", "--quiet", file, &reference, "--not", &fetched,
    ];
    let bundled = run_isolated(repo, &bundle).await;
    run_isolated(repo, &["update-ref", "-d", &reference]).await?;
    bundled.map(drop)
}

/// Fetch `commit` from the bundle `file` that [`bundle_carried`] wrote into
/// `repo` under its carried ref, after the origin's `branch`, which holds
/// the history the bundle leaves out; refused for a bundle of another
/// commit.
pub async fn fetch_carried(
    repo: &Path,
    file: &Path,
    (commit, branch): (&CommitId, &str),
) -> Result<()> {
    let path = file.to_str().context("bundle path is not UTF-8")?;
    let heads = run_isolated(repo, &["bundle", "list-heads", path]).await?;
    let bundled = (heads.lines()).find_map(|line| {
        line.split_once(' ')
            .filter(|(id, _)| *id == commit.as_str())
    });
    let (_, reference) = bundled.with_context(|| format!("the bundle holds no commit {commit}"))?;
    fetch_branch(repo, branch).await?;
    let refspec = format!("{reference}:{}", carried_reference(commit));
    let fetch = [FETCH_SAFE_ARGS, &["--", path, refspec.as_str()]].concat();
    run_isolated(repo, &fetch).await.map(drop)
}

#[cfg(test)]
mod tests;
