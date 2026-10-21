//! The detached worktree a repository's seed builds the default branch in,
//! and the file lists that decide which build outputs a workspace may reuse.
//! Worktrunk manages branch worktrees; the seed has no branch, so plain Git
//! creates it, locked so pruning and adoption leave it alone.
use std::{collections::HashSet, path::Path};

use anyhow::Result;

use super::{run, run_isolated, run_without_submodules};

/// The commit a local branch points at.
pub async fn branch_commit(repo: &Path, branch: &str) -> Result<String> {
    let reference = format!("{}^{{commit}}", super::local_ref(branch));
    Ok(run(repo, &["rev-parse", "--verify", "--quiet", &reference])
        .await?
        .trim()
        .to_owned())
}

pub async fn add_worktree(repo: &Path, path: &Path, commit: &str, reason: &str) -> Result<()> {
    let path = path.to_string_lossy();
    run_without_submodules(
        repo,
        &[
            "worktree", "add", "--detach", "--lock", "--reason", reason, "--", &path, commit,
        ],
    )
    .await
    .map(drop)
}

/// Move the seed to `commit`, discarding tracked edits a build made. Git
/// rewrites only the files that differ, so the others keep their times and
/// the next seed build stays incremental.
pub async fn check_out(worktree: &Path, commit: &str) -> Result<()> {
    run_without_submodules(worktree, &["checkout", "--detach", "--force", commit, "--"])
        .await
        .map(drop)
}

pub async fn remove_worktree(repo: &Path, path: &Path) -> Result<()> {
    let path = path.to_string_lossy();
    run_isolated(
        repo,
        &["worktree", "remove", "--force", "--force", "--", &path],
    )
    .await
    .map(drop)
}

pub async fn head(worktree: &Path) -> Result<String> {
    Ok(run(worktree, &["rev-parse", "--verify", "HEAD"])
        .await?
        .trim()
        .to_owned())
}

/// Tracked files whose content differs between two commits.
pub async fn changed_between(repo: &Path, from: &str, to: &str) -> Result<HashSet<String>> {
    let listing = run(
        repo,
        &["diff", "--name-only", "-z", "--no-renames", from, to],
    )
    .await?;
    Ok(names(&listing).collect())
}

/// Tracked files whose content matches HEAD.
pub async fn unmodified_files(worktree: &Path) -> Result<Vec<String>> {
    let tracked = run(worktree, &["ls-files", "-z"]).await?;
    let modified = run(
        worktree,
        &["diff", "--name-only", "-z", "--no-renames", "HEAD"],
    )
    .await?;
    let modified: HashSet<String> = names(&modified).collect();
    Ok(names(&tracked)
        .filter(|name| !modified.contains(name))
        .collect())
}

/// Which of the worktree-relative `paths` the worktree's index tracks.
pub async fn tracked(worktree: &Path, paths: &[&str]) -> Result<HashSet<String>> {
    let pathspecs: Vec<String> = paths
        .iter()
        .map(|path| format!(":(literal){path}"))
        .collect();
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend(pathspecs.iter().map(String::as_str));
    Ok(names(&run(worktree, &args).await?).collect())
}

fn names(listing: &str) -> impl Iterator<Item = String> + '_ {
    listing
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}
