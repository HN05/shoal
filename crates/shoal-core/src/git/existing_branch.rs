//! Existing branches a workspace can open, and the checkout rule for them.
use anyhow::{Context, Result};
use shoal_refs::BranchName;

use crate::git;
pub use crate::model::Branch;
use std::path::Path;

/// Operations on the client type [`Branch`] that only the `shoal` command uses.
pub trait BranchExt {
    fn selector(&self) -> String;
}

impl BranchExt for Branch {
    fn selector(&self) -> String {
        match &self.remote {
            Some(remote) => git::remote_ref(remote, self.name.as_str()),
            None => git::local_ref(&self.name.clone()),
        }
    }
}

/// A worktree for `name` can be created only where no checkout already has it.
pub async fn ensure_not_checked_out(repo: &std::path::Path, name: &BranchName) -> Result<()> {
    if let Some(tree) = git::checkout_of(repo, name, git::run).await? {
        anyhow::bail!(
            "branch {name} is already checked out at {}; Shoal cannot create another worktree for it; use shoal adopt for an unmanaged linked worktree",
            tree.path.display()
        );
    }
    Ok(())
}

/// Local branches and advertised remote heads, including never-fetched branches.
pub async fn list(repo: &Path) -> Result<Vec<Branch>> {
    let mut branches: Vec<_> = git::run_isolated(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:strip=2)",
            git::LOCAL_REFS,
        ],
    )
    .await?
    .lines()
    .map(|name| Branch {
        name: name.into(),
        remote: None,
    })
    .collect();
    for remote in git::remotes(repo, git::run_isolated).await?.lines() {
        let heads = git::run_isolated(repo, &["ls-remote", "--heads", "--", remote])
            .await.with_context(|| format!("could not list branches from {remote}; use --branch <local-branch> to work offline"))?;
        branches.extend(heads.lines().filter_map(|line| {
            let (_, reference) = line.split_once('\t')?;
            Some(Branch {
                name: git::strip_local(reference)?.into(),
                remote: Some(remote.into()),
            })
        }));
    }
    Ok(branches)
}

pub async fn upstream_ref(repo: &Path, name: &BranchName) -> Result<String> {
    git::run_isolated(
        repo,
        &[
            "for-each-ref",
            "--format=%(upstream)",
            &git::local_ref(name),
        ],
    )
    .await
}

/// The remote-tracking ref of `default`'s upstream as its remote's fetch
/// refspec maps it, when a fetch has stored it.
pub async fn tracking_ref(path: &Path, default: &BranchName) -> Result<Option<String>> {
    let tracking = upstream_ref(path, default).await?;
    let tracking = tracking.trim();
    if tracking.is_empty() {
        return Ok(None);
    }
    Ok(
        match git::ref_exists(path, tracking, git::isolated_command).await? {
            true => Some(tracking.to_owned()),
            false => None,
        },
    )
}

pub async fn track(repo: &Path, branch: &BranchName, tracking: &str) -> Result<String> {
    let args = ["branch", "--track", "--", branch.as_str(), tracking];
    git::run_isolated(repo, &args).await
}
