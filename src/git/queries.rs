//! Value queries with caller-selected Git invocation policy.
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::run;

/// Read an object or revision without peeling it to a commit.
pub async fn revision(
    repo: &Path,
    reference: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["rev-parse", reference]).await
}

/// Require one revision, without changing its object type.
pub async fn verify_revision(
    repo: &Path,
    reference: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["rev-parse", "--verify", reference]).await
}

pub async fn symbolic_revision(
    repo: &Path,
    reference: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["rev-parse", "--symbolic-full-name", reference]).await
}

/// Detached HEAD returns an empty branch name.
pub async fn current_branch(repo: &Path) -> Result<String> {
    run(repo, &["branch", "--show-current"]).await
}

pub async fn worktree_root(repo: &Path) -> Result<String> {
    run(repo, &["rev-parse", "--show-toplevel"]).await
}

pub async fn git_dir(repo: &Path) -> Result<PathBuf> {
    let dir = run(repo, &["rev-parse", "--path-format=absolute", "--git-dir"]).await?;
    Ok(std::fs::canonicalize(dir.trim())?)
}

pub async fn metadata_path(
    repo: &Path,
    name: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["rev-parse", "--git-path", name]).await
}

pub async fn remotes(
    repo: &Path,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["remote"]).await
}

pub async fn remote_url(repo: &Path, remote: &str) -> Result<String> {
    run(repo, &["remote", "get-url", remote]).await
}

pub async fn latest_subject(repo: &Path) -> Result<String> {
    run(repo, &["log", "-1", "--format=%s"]).await
}
