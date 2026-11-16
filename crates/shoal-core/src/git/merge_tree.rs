//! Merges with `git merge-tree --write-tree`, which writes objects only,
//! never the index, worktree or refs.
use std::path::Path;

use anyhow::Result;

use super::version::{self, Release};

/// The first Git whose `merge-tree` takes `--write-tree`.
const WRITE_TREE: Release = (2, 38);

/// What `git merge-tree --write-tree` reported.
pub(crate) enum Merge {
    /// The merge is clean; Git printed this.
    Clean(String),
    /// The merge conflicts; Git printed this, starting with the tree ID.
    Conflicted(Vec<u8>),
}

/// `git merge-tree --write-tree` with `args`, which exits 0 for a clean
/// merge and 1 for a conflicted one. Git also exits 1 for some failures,
/// such as a revision it cannot resolve, which print no tree ID: those, and
/// any other exit, are errors.
pub(crate) async fn write_tree(repo: &Path, args: &[&str]) -> Result<Merge> {
    let mut command = super::isolated_command_without_submodules(repo);
    command.args(["merge-tree", "--write-tree"]).args(args);
    match super::bounded(command).capture().await {
        Ok(output) if output.status.code() == Some(1) && starts_with_tree_id(&output.stdout) => {
            Ok(Merge::Conflicted(output.stdout))
        }
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

/// Whether `stdout`'s first line or `-z` field is an object ID.
fn starts_with_tree_id(stdout: &[u8]) -> bool {
    let first = stdout.split(|&byte| matches!(byte, b'\n' | b'\0')).next();
    first.is_some_and(|id| matches!(id.len(), 40 | 64) && id.iter().all(u8::is_ascii_hexdigit))
}

#[cfg(test)]
mod tests;
