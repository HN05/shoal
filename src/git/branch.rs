//! Branch names, renaming and checkout fast-forwards.
use std::path::Path;

use anyhow::Result;

use crate::git;

/// Local and remote branch names that can conflict with a new local branch.
pub async fn names_in_use(repo: &Path) -> Result<Vec<String>> {
    let refs = git::run(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            git::LOCAL_REFS,
            git::REMOTE_REFS,
        ],
    )
    .await?;
    Ok(refs
        .lines()
        .filter_map(|reference| {
            git::strip_local(reference).or_else(|| {
                git::strip_remote(reference)?
                    .split_once('/')
                    .map(|(_, branch)| branch)
            })
        })
        .map(str::to_owned)
        .collect())
}

/// Create a local branch at `commit`; an existing branch is an error.
pub async fn create(repo: &Path, name: &str, commit: &str) -> Result<String> {
    git::run_isolated(repo, &["branch", "--no-track", "--", name, commit]).await
}

/// Delete a local branch only while it still points at `commit`, so a
/// branch that moved meanwhile stays. The caller checks that no worktree
/// has it checked out.
pub async fn delete_at(repo: &Path, name: &str, commit: &str) -> Result<String> {
    git::run_isolated(repo, &["update-ref", "-d", &git::local_ref(name), commit]).await
}

pub async fn rename(repo: &Path, from: &str, to: &str) -> Result<String> {
    git::run_isolated(repo, &["branch", "-m", "--", from, to]).await
}

/// Fast-forward a checkout without recursive submodule updates.
pub async fn fast_forward(repo: &Path, commit: &str) -> Result<String> {
    git::run_without_submodules(
        repo,
        &[
            "merge",
            "--ff-only",
            "--no-edit",
            "--no-stat",
            "--no-overwrite-ignore",
            commit,
        ],
    )
    .await
}
