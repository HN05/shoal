//! Merge conflict detection without touching the worktree.
use std::path::Path;

use anyhow::{Context, Result};

use crate::{git, git::merge_tree::Merge};

/// The paths a merge of `head` and `target` leaves conflicted; empty when it
/// is clean.
pub async fn merge_conflicts(repo: &Path, head: &str, target: &str) -> Result<Vec<String>> {
    let args = ["--name-only", "--no-messages", "-z", head, target];
    let stdout = match git::merge_tree::write_tree(repo, &args)
        .await
        .context("check merge conflicts")?
    {
        Merge::Clean(_) => return Ok(Vec::new()),
        Merge::Conflicted(stdout) => stdout,
    };
    let stdout = String::from_utf8(stdout).context("Git paths are not UTF-8")?;
    Ok(conflicted_paths(&stdout))
}

/// The paths matching `patterns`, globs relative to the repository root,
/// that both `head` and `target` changed since their merge base, whether or
/// not their changes merge cleanly.
pub async fn changed_on_both_sides(
    repo: &Path,
    head: &str,
    target: &str,
    patterns: &[String],
) -> Result<Vec<String>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let base = git::diff::merge_base_of(repo, head, target).await?;
    let theirs = changed_paths(repo, base.as_str(), target, patterns).await?;
    let ours = changed_paths(repo, base.as_str(), head, patterns).await?;
    Ok(ours
        .into_iter()
        .filter(|path| theirs.contains(path))
        .collect())
}

/// The paths matching `patterns` that differ between `from` and `to`.
async fn changed_paths(
    repo: &Path,
    from: &str,
    to: &str,
    patterns: &[String],
) -> Result<Vec<String>> {
    let mut args = vec!["diff", "--name-only", "--no-renames", "-z", from, to, "--"];
    let pathspecs: Vec<String> = patterns.iter().map(|p| format!(":(top,glob){p}")).collect();
    args.extend(pathspecs.iter().map(String::as_str));
    let output = git::run_isolated(repo, &args).await?;
    Ok(output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The paths after the tree ID in `-z` output.
fn conflicted_paths(stdout: &str) -> Vec<String> {
    stdout
        .split('\0')
        .skip(1)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests;
