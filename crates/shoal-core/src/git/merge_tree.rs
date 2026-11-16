//! Merges with `git merge-tree --write-tree`, which writes objects only,
//! never the index, worktree or refs.
use std::{path::Path, process::Output};

use anyhow::Result;

use super::version::{self, Release};

/// The first Git whose `merge-tree` takes `--write-tree`.
const WRITE_TREE: Release = (2, 38);

/// What `git merge-tree --write-tree` reported.
pub(crate) enum Merge {
    /// The merge is clean; Git printed this.
    Clean(String),
    /// The merge conflicts, as Git also reports for some failures.
    Conflicted(Output),
}

/// `git merge-tree --write-tree` with `args`, which exits 0 for a clean
/// merge and 1 for a conflicted one; any other exit is an error.
pub(crate) async fn write_tree(repo: &Path, args: &[&str]) -> Result<Merge> {
    let mut command = super::isolated_command_without_submodules(repo);
    command.args(["merge-tree", "--write-tree"]).args(args);
    match super::bounded(command).capture().await {
        Ok(output) if output.status.code() == Some(1) => Ok(Merge::Conflicted(output)),
        output => version::checked_output("git merge-tree", output, WRITE_TREE)
            .await
            .map(Merge::Clean),
    }
}

/// The tree of merging `theirs` into `ours`, or `None` when it conflicts.
pub(crate) async fn merged_tree(repo: &Path, ours: &str, theirs: &str) -> Result<Option<String>> {
    Ok(
        match write_tree(repo, &["--no-messages", ours, theirs]).await? {
            Merge::Clean(stdout) => stdout.lines().next().map(str::to_owned),
            Merge::Conflicted(_) => None,
        },
    )
}

#[cfg(test)]
mod tests;
