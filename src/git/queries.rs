//! Value queries with caller-selected Git invocation policy.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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

/// HEAD's commit and the branch it is on, `None` when detached, read with
/// one Git process instead of two.
pub async fn head_and_branch(repo: &Path) -> Result<(String, Option<String>)> {
    let output = run(repo, &["rev-parse", "HEAD", "--symbolic-full-name", "HEAD"]).await?;
    let mut lines = output.lines();
    let commit = lines.next().context("git rev-parse printed no commit")?;
    let branch = lines
        .next()
        .and_then(|name| name.strip_prefix("refs/heads/"));
    Ok((commit.to_owned(), branch.map(str::to_owned)))
}

/// Detached HEAD returns an empty branch name.
pub async fn current_branch(repo: &Path) -> Result<String> {
    run(repo, &["branch", "--show-current"]).await
}

pub async fn worktree_root(repo: &Path) -> Result<String> {
    run(repo, &["rev-parse", "--show-toplevel"]).await
}

/// Whether the checkout at `path` has a `.git` directory, yet its Git config
/// sets `core.bare`, so Git refuses every working-tree command there. Running
/// `git init --bare` with an inherited `GIT_DIR` leaves a checkout this way.
pub async fn is_bare_with_work_tree(path: &Path) -> Result<bool> {
    if !path.join(".git").is_dir() {
        return Ok(false);
    }
    Ok(run(path, &["rev-parse", "--is-bare-repository"])
        .await?
        .trim()
        == "true")
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

/// The remote a push of `branch` goes to: `branch.<name>.pushRemote`, else
/// `remote.pushDefault`, else `origin`.
pub async fn push_remote(repo: &Path, branch: &str) -> Result<String> {
    for key in [
        format!("branch.{branch}.pushRemote"),
        "remote.pushDefault".into(),
    ] {
        let value = run(repo, &["config", "--default", "", "--get", &key]).await?;
        if !value.trim().is_empty() {
            return Ok(value.trim().to_owned());
        }
    }
    Ok("origin".into())
}

pub async fn latest_subject(repo: &Path) -> Result<String> {
    run(repo, &["log", "-1", "--format=%s"]).await
}

#[cfg(test)]
mod tests;
