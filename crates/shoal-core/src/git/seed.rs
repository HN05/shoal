//! The detached worktree a repository's seed builds the default branch in,
//! and the file lists that decide which build outputs a workspace may reuse.
//! It is locked so pruning and adoption leave it alone.
use std::{
    collections::HashSet,
    path::{Component, Path},
};

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

/// The `outputs` of the worktree that hold no tracked file, so deleting
/// them leaves the worktree as clean as it was.
pub async fn untracked_outputs(worktree: &Path, outputs: &[String]) -> Result<Vec<String>> {
    if outputs.is_empty() {
        return Ok(Vec::new());
    }
    let pathspecs: Vec<String> = outputs
        .iter()
        .map(|output| format!(":(literal){output}"))
        .collect();
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend(pathspecs.iter().map(String::as_str));
    let listing = super::run_isolated(worktree, &args).await?;
    let tracked: Vec<String> = names(&listing).collect();
    Ok(outputs
        .iter()
        .filter(|output| !tracked.iter().any(|file| holds(output, file)))
        .cloned()
        .collect())
}

/// Whether the tracked `file` is or lies inside `output`, comparing path
/// components, so spellings such as `target/` or `./web//node_modules`
/// match what Git lists.
fn holds(output: &str, file: &str) -> bool {
    let output: Vec<_> = Path::new(output)
        .components()
        .filter(|component| *component != Component::CurDir)
        .collect();
    !output.is_empty() && Path::new(file).components().take(output.len()).eq(output)
}

fn names(listing: &str) -> impl Iterator<Item = String> + '_ {
    listing
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests;
