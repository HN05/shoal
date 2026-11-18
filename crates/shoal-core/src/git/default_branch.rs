//! Resolve repository policy without assuming a particular branch spelling.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use shoal_refs::BranchName;

use crate::git;

#[derive(Debug, Clone, Copy)]
pub enum DefaultBranchLookup {
    Cached,
    Discover,
}

/// The remote among the repository's `remotes` whose HEAD names the
/// default branch: `origin`, or the only remote; `None` without remotes.
fn default_remote(remotes: &[String]) -> Result<Option<&str>> {
    if remotes.is_empty() {
        return Ok(None);
    }
    if remotes.iter().any(|remote| remote == "origin") {
        return Ok(Some("origin"));
    }
    ensure!(
        remotes.len() == 1,
        "repository default remote is ambiguous (no origin); select a starting ref with --base"
    );
    Ok(Some(&remotes[0]))
}

/// The default branch, with the remote whose HEAD names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultBranch {
    /// `None` without remotes, where the local HEAD names the branch.
    pub remote: Option<String>,
    pub name: BranchName,
}

/// The default branch's name, as [`locate`] finds it.
pub async fn resolve(repo: &Path, lookup: DefaultBranchLookup) -> Result<BranchName> {
    Ok(locate(repo, lookup).await?.name)
}

/// Read cached remote HEAD. Branch refresh may discover and cache a missing one;
/// cleanup only reads local metadata and never contacts a remote.
pub async fn locate(repo: &Path, lookup: DefaultBranchLookup) -> Result<DefaultBranch> {
    locate_among(repo, &git::remote_names(repo).await?, lookup).await
}

/// The default branch as [`locate`] finds it, in the repository at `repo`
/// whose remotes the caller listed as `remotes`.
pub async fn locate_among(
    repo: &Path,
    remotes: &[String],
    lookup: DefaultBranchLookup,
) -> Result<DefaultBranch> {
    let Some(remote) = default_remote(remotes)? else {
        let head = git::head_branch(repo)
            .await
            .context("local repository has no default branch; select a starting ref with --base")?;
        let name = head.context("repository HEAD does not name a local branch")?;
        return Ok(DefaultBranch { remote: None, name });
    };
    let name = remote_head(repo, remote, lookup).await?;
    Ok(DefaultBranch {
        remote: Some(remote.to_owned()),
        name,
    })
}

/// The branch `remote`'s cached HEAD names, discovered and cached when
/// `lookup` allows.
async fn remote_head(repo: &Path, remote: &str, lookup: DefaultBranchLookup) -> Result<BranchName> {
    let head = git::remote_ref(remote, "HEAD");
    let prefix = git::remote_ref(remote, "");
    if let Some(target) = git::refs::symbolic_target(repo, &head).await?
        && let Some(branch) = target.strip_prefix(&prefix)
    {
        return Ok(branch.into());
    }
    ensure!(
        matches!(lookup, DefaultBranchLookup::Discover),
        "repository default branch is unknown; refresh {remote}/HEAD with git remote set-head"
    );
    let advertised = git::run_isolated(repo, &["ls-remote", "--symref", "--", remote, "HEAD"])
        .await
        .with_context(|| {
            format!(
                "discover {remote}'s default branch; use --base to select a starting point explicitly"
            )
        })?;
    let branch = advertised
        .lines()
        .find_map(|line| git::strip_local(line.strip_prefix("ref: ")?)?.strip_suffix("\tHEAD"))
        .context(
            "remote HEAD does not advertise a default branch; select a starting ref with --base",
        )?;
    git::run(
        repo,
        &["symbolic-ref", &head, &git::remote_ref(remote, branch)],
    )
    .await?;
    Ok(branch.into())
}
