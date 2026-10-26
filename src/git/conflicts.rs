//! Merge conflict detection without touching the worktree.
use std::path::Path;

use anyhow::{Context, Result};

use crate::{git, subprocess};

/// The paths a merge of `head` and `target` leaves conflicted; empty when it
/// is clean. `git merge-tree` writes objects only, never the index, worktree
/// or refs.
pub(crate) async fn merge_conflicts(repo: &Path, head: &str, target: &str) -> Result<Vec<String>> {
    let mut command = git::isolated_command_without_submodules(repo);
    command.args([
        "merge-tree",
        "--write-tree",
        "--name-only",
        "--no-messages",
        "-z",
        head,
        target,
    ]);
    let output = subprocess::Run::new(command).capture().await;
    if output.as_ref().ok().and_then(|output| output.status.code()) != Some(1) {
        subprocess::checked_output("git merge-tree", output)
            .context("check merge conflicts; Git 2.38 or newer is required")?;
        return Ok(Vec::new());
    }
    let output = output?;
    let stdout = String::from_utf8(output.stdout).context("Git paths are not UTF-8")?;
    // Git also exits 1 for some failures, which print no tree or paths.
    conflicted_paths(&stdout).with_context(|| {
        format!(
            "git merge-tree failed: {}",
            subprocess::diagnostic(&output.stderr)
        )
    })
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
