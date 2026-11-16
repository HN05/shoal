//! A removal of a linked worktree that stopped before Git finished, as when
//! its `worktree remove` was stopped: the marker that says one began, and
//! putting back what it deleted that a later decision about the worktree
//! must not mistake for anyone's work, its `.git` link and tracked files.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::fsutil::{self, off_runtime};

/// The file in a worktree's admin directory that says a removal began
/// deleting the worktree, holding the text of its `.git` link. Git deletes
/// it with the admin directory once the removal finishes.
const MARKER: &str = "shoal-removal";

/// Record in `admin_dir` that a removal of its worktree, whose `.git` link
/// reads `link`, begins deleting it.
pub(super) async fn mark(admin_dir: &Path, link: String) -> Result<()> {
    let marker = admin_dir.join(MARKER);
    off_runtime::run(move || {
        // Renamed into place, so a crash never leaves a truncated link.
        let partial = marker.with_extension("partial");
        std::fs::write(&partial, link)
            .and_then(|()| std::fs::rename(&partial, &marker))
            .with_context(|| format!("write {}", marker.display()))
    })
    .await
}

/// Forget that a removal began, once it ended having deleted nothing.
pub(super) async fn unmark(admin_dir: &Path) -> Result<()> {
    let marker = admin_dir.join(MARKER);
    off_runtime::run(move || match std::fs::remove_file(&marker) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("remove {}", marker.display()))
        }
        _ => Ok(()),
    })
    .await
}

/// Put back what a removal of the worktree at `path` deleted before it
/// stopped, when the marker in its admin directory `admin_dir` says one
/// began: the `.git` link, then from the index each tracked file, so that
/// only the ignored and untracked files the removal took stay gone. Then
/// forget the removal. Returns whether one had begun.
pub async fn restore(path: &Path, admin_dir: &Path) -> Result<bool> {
    let Some(link) = marked_link(admin_dir).await? else {
        return Ok(false);
    };
    if off_runtime::is_dir(path).await? {
        ensure_registered_at(admin_dir, path).await?;
        restore_link(path, link).await?;
        ensure_linked_to(path, admin_dir).await?;
        restore_tracked_files(path).await?;
    }
    unmark(admin_dir).await?;
    Ok(true)
}

/// Whether the marker in `admin_dir` says a removal of its worktree began
/// and has not been forgotten.
pub async fn began(admin_dir: &Path) -> Result<bool> {
    Ok(marked_link(admin_dir).await?.is_some())
}

/// The `.git` link text the marker in `admin_dir` keeps; `None` when no
/// removal began.
async fn marked_link(admin_dir: &Path) -> Result<Option<String>> {
    let marker = admin_dir.join(MARKER);
    off_runtime::run(move || Ok(fsutil::read_optional(&marker)?)).await
}

/// Refuse to touch the directory at `path` unless the admin directory
/// `admin_dir` names it as its worktree, as Git records in its `gitdir`
/// file, relative to it when `worktree.useRelativePaths` is set.
async fn ensure_registered_at(admin_dir: &Path, path: &Path) -> Result<()> {
    let gitdir = admin_dir.join("gitdir");
    let text = off_runtime::run(move || {
        std::fs::read_to_string(&gitdir).with_context(|| format!("read {}", gitdir.display()))
    })
    .await?;
    let named = admin_dir.join(text.trim_end_matches('\n'));
    anyhow::ensure!(
        super::resolved(&named).await == super::resolved(&path.join(".git")).await,
        "{} is no longer the worktree of {}",
        path.display(),
        admin_dir.display()
    );
    Ok(())
}

/// Write the `.git` link of the worktree at `path` back with `link` unless
/// it is there.
async fn restore_link(path: &Path, link: String) -> Result<()> {
    let file = path.join(".git");
    off_runtime::run(move || {
        if !file.try_exists()? {
            std::fs::write(&file, link).with_context(|| format!("restore {}", file.display()))?;
        }
        Ok(())
    })
    .await
}

/// Refuse a worktree at `path` whose `.git` link names another admin
/// directory than `admin_dir`, as when another checkout took the path.
async fn ensure_linked_to(path: &Path, admin_dir: &Path) -> Result<()> {
    let named = {
        let path = path.to_owned();
        off_runtime::run(move || super::linked_admin_dir(&path)).await?
    };
    let linked = match named {
        Some(named) => super::resolved(&named).await == super::resolved(admin_dir).await,
        None => false,
    };
    anyhow::ensure!(
        linked,
        "{} is no longer the worktree whose removal stopped",
        path.display()
    );
    Ok(())
}

/// Check the tracked files missing from the worktree at `path` out of its
/// index again.
async fn restore_tracked_files(path: &Path) -> Result<()> {
    let deleted = crate::git::run_isolated(path, &["ls-files", "-z", "--deleted"]).await?;
    if deleted.is_empty() {
        return Ok(());
    }
    let mut command = crate::git::isolated_command(path);
    command.args(["checkout-index", "-z", "--stdin"]);
    crate::git::bounded(command)
        .input(deleted)
        .output()
        .await
        .with_context(|| format!("restore the tracked files of {}", path.display()))
        .map(drop)
}

/// The admin directory a worktree's `.git` link reading `text` names, for
/// the worktree at `path`.
pub(super) fn admin_dir(path: &Path, text: &str) -> Option<PathBuf> {
    let named = text.strip_prefix("gitdir: ")?;
    Some(path.join(named.trim_end_matches('\n')))
}

#[cfg(test)]
mod tests;
