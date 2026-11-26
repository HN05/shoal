//! A worktree removal that stopped before Git finished: the marker that says
//! one began, and putting back the `.git` link and tracked files it deleted.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::fsutil::{self, off_runtime};

/// The error of a removal of the worktree at its path that stopped before
/// Git finished and could not put back what it deleted, leaving its marker
/// for the next removal, which a caller tells from other failures by it.
#[derive(Debug)]
pub struct Unrestored(pub(crate) PathBuf);

impl std::fmt::Display for Unrestored {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "removing the worktree at {} stopped partway",
            self.0.display()
        )
    }
}

/// The file in a worktree's admin directory that says a removal began
/// deleting the worktree, holding a [`Marker`]. Git deletes it with the
/// admin directory once the removal finishes.
const MARKER: &str = "shoal-removal";

#[derive(serde::Serialize, serde::Deserialize)]
struct Marker {
    /// The text of the worktree's `.git` link.
    link: String,
    /// The tracked files already missing from the worktree, which are not
    /// the removal's doing.
    deleted_before: BTreeSet<String>,
}

/// Record in `admin_dir` that a removal of the worktree at `path`, whose
/// `.git` link reads `link`, begins deleting it, with the tracked files
/// already missing there.
pub(super) async fn mark(admin_dir: &Path, path: &Path, link: String) -> Result<()> {
    let deleted_before = deleted_files(path).await?;
    let text = serde_json::to_string(&Marker {
        link,
        deleted_before,
    })?;
    let marker = admin_dir.join(MARKER);
    off_runtime::run(move || {
        // Renamed into place, so a crash never leaves a truncated marker.
        let partial = marker.with_extension("partial");
        std::fs::write(&partial, text)
            .and_then(|()| std::fs::rename(&partial, &marker))
            .with_context(|| format!("write {}", marker.display()))
    })
    .await
}

/// Forget that a removal began: it ended having deleted nothing, or what
/// it deleted is no longer told apart from later changes.
pub async fn forget(admin_dir: &Path) -> Result<()> {
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
/// began: the `.git` link, then from the index each tracked file it
/// deleted, so that only the ignored and untracked files the removal took,
/// and the tracked files missing before it began, stay gone. Then forget
/// the removal. Returns whether one had begun.
pub async fn restore(path: &Path, admin_dir: &Path) -> Result<bool> {
    let Some(marker) = read_marker(admin_dir).await? else {
        return Ok(false);
    };
    if off_runtime::is_dir(path).await? {
        ensure_registered_at(admin_dir, path).await?;
        super::restore_link(path, marker.link).await?;
        ensure_linked_to(path, admin_dir).await?;
        restore_tracked_files(path, &marker.deleted_before).await?;
    }
    forget(admin_dir).await?;
    Ok(true)
}

/// Whether the marker in `admin_dir` says a removal of its worktree began
/// and has not been forgotten.
pub async fn began(admin_dir: &Path) -> Result<bool> {
    Ok(read_marker(admin_dir).await?.is_some())
}

/// The marker in `admin_dir`; `None` when no removal began.
async fn read_marker(admin_dir: &Path) -> Result<Option<Marker>> {
    let marker = admin_dir.join(MARKER);
    let text = {
        let marker = marker.clone();
        off_runtime::run(move || Ok(fsutil::read_optional(&marker)?)).await?
    };
    text.map(|text| serde_json::from_str(&text))
        .transpose()
        .with_context(|| format!("read {}", marker.display()))
}

/// Refuse to touch the directory at `path` unless the admin directory
/// `admin_dir` names it as its worktree, as Git records in its `gitdir`
/// file, relative to it when `worktree.useRelativePaths` is set.
async fn ensure_registered_at(admin_dir: &Path, path: &Path) -> Result<()> {
    anyhow::ensure!(
        super::registered_at(admin_dir, path).await?,
        "{} is no longer the worktree of {}",
        path.display(),
        admin_dir.display()
    );
    Ok(())
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
/// index again, except those `deleted_before`.
async fn restore_tracked_files(path: &Path, deleted_before: &BTreeSet<String>) -> Result<()> {
    let deleted = deleted_files(path).await?;
    let restored: Vec<&str> = (deleted.difference(deleted_before))
        .map(String::as_str)
        .collect();
    if restored.is_empty() {
        return Ok(());
    }
    let mut command = crate::git::isolated_command(path);
    command.args(["checkout-index", "-z", "--stdin"]);
    crate::git::bounded(command)
        .input(restored.join("\0"))
        .output()
        .await
        .with_context(|| format!("restore the tracked files of {}", path.display()))
        .map(drop)
}

async fn deleted_files(path: &Path) -> Result<BTreeSet<String>> {
    let listed = crate::git::run_isolated(path, &["ls-files", "-z", "--deleted"]).await?;
    Ok((listed.split_terminator('\0')).map(str::to_owned).collect())
}

/// The admin directory a worktree's `.git` link reading `text` names, for
/// the worktree at `path`.
pub(super) fn admin_dir(path: &Path, text: &str) -> Option<PathBuf> {
    let named = text.strip_prefix("gitdir: ")?;
    Some(path.join(named.trim_end_matches('\n')))
}

#[cfg(test)]
mod tests;
