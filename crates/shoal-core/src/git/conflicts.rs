//! Merge conflict detection without touching the worktree.
use std::path::Path;

use anyhow::{Context, Result};

use crate::{git, git::merge_tree::Merge, subprocess};

/// The paths a merge of `head` and `target` leaves conflicted; empty when it
/// is clean.
pub async fn merge_conflicts(repo: &Path, head: &str, target: &str) -> Result<Vec<String>> {
    let args = ["--name-only", "--no-messages", "-z", head, target];
    let output = match git::merge_tree::write_tree(repo, &args)
        .await
        .context("check merge conflicts")?
    {
        Merge::Clean(_) => return Ok(Vec::new()),
        Merge::Conflicted(output) => output,
    };
    let stdout = String::from_utf8(output.stdout).context("Git paths are not UTF-8")?;
    // Git also exits 1 for some failures, which print no tree or paths.
    conflicted_paths(&stdout).with_context(|| {
        format!(
            "git merge-tree failed: {}",
            subprocess::diagnostic(&output.stderr)
        )
    })
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

/// The paths after the tree ID in `-z` output, or `None` when it is not a
/// conflict result.
fn conflicted_paths(stdout: &str) -> Option<Vec<String>> {
    let mut fields = stdout.split('\0');
    let tree = fields.next()?;
    let tree_id = matches!(tree.len(), 40 | 64) && tree.bytes().all(|b| b.is_ascii_hexdigit());
    let paths: Vec<_> = fields
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    (tree_id && !paths.is_empty()).then_some(paths)
}

#[cfg(test)]
mod tests;
