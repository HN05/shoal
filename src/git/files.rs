//! Files of one name anywhere in a worktree or a revision.
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::run;

/// Every file named `name` in the worktree, tracked, untracked or ignored,
/// since generated files are often ignored, and tracked in its submodules.
pub async fn in_worktree(worktree: &Path, name: &str) -> Result<Vec<PathBuf>> {
    let pathspec = pathspec(name);
    let worktree_files = run(
        worktree,
        &["ls-files", "-z", "--cached", "--others", "--", &pathspec],
    )
    .await?;
    // Recursing into submodules lists only tracked files.
    let submodule_files = run(
        worktree,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--recurse-submodules",
            "--",
            &pathspec,
        ],
    )
    .await?;
    Ok(paths(&[worktree_files, submodule_files].concat())
        .map(|path| worktree.join(path))
        .collect())
}

/// The content of every file named `name` at `revision`.
pub async fn at_revision(repo: &Path, revision: &str, name: &str) -> Result<Vec<String>> {
    // `ls-tree` takes no glob pathspecs.
    let listing = run(repo, &["ls-tree", "-r", "-z", "--name-only", revision]).await?;
    let mut contents = Vec::new();
    for path in paths(&listing).filter(|path| Path::new(path).file_name() == Some(name.as_ref())) {
        contents.push(run(repo, &["cat-file", "blob", &format!("{revision}:{path}")]).await?);
    }
    Ok(contents)
}

fn pathspec(name: &str) -> String {
    format!(":(glob)**/{name}")
}

fn paths(listing: &str) -> impl Iterator<Item = &str> {
    // `ls-files` repeats a path for each unmerged stage.
    let mut seen = std::collections::HashSet::new();
    listing
        .split('\0')
        .filter(move |path| !path.is_empty() && seen.insert(*path))
}

#[cfg(test)]
mod tests;
