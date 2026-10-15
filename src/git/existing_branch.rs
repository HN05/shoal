//! Existing branches a workspace can open, and the checkout rule for them.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::git;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub remote: Option<String>,
}

impl Branch {
    pub fn selector(&self) -> String {
        match &self.remote {
            Some(remote) => git::remote_ref(remote, &self.name),
            None => git::local_ref(&self.name),
        }
    }
}

/// A worktree for `name` can be created only where no checkout already has it.
pub async fn ensure_not_checked_out(repo: &std::path::Path, name: &str) -> Result<()> {
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
            .await.with_context(|| format!("could not list branches from {remote}; use --existing <local-branch> to work offline"))?;
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

pub async fn upstream_ref(repo: &Path, name: &str) -> Result<String> {
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

pub async fn track(repo: &Path, branch: &str, tracking: &str) -> Result<String> {
    git::run_isolated(repo, &["branch", "--track", "--", branch, tracking]).await
}
