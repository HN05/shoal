//! Filesystem policies used when claiming and verifying workspace ownership.
use super::identity::directory_identity;
use crate::paths::Paths;
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Whether a canonical candidate contains home or Shoal state, including equality.
/// Descendants of protected directories are left to the caller's policy.
pub(super) fn contains_protected_directory(path: &Path, paths: &Paths) -> Result<bool> {
    for protected in [&paths.home, &paths.state] {
        if fs::canonicalize(protected)?.starts_with(path) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Normalize an absolute path, resolving existing symlinks before `..`.
/// Missing components are retained and `..` is resolved lexically; dangling
/// symlinks and filesystem errors other than missing components are rejected.
pub(crate) fn canonical_with_missing_tail(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "path must be absolute");
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => {
                result.push(other);
                match fs::symlink_metadata(&result) {
                    Ok(_) => result = fs::canonicalize(&result)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Ok(result)
}

/// Normalize a recorded path for comparison with Git even when one or more
/// ancestors were deleted, preserving the leaf instead of resolving it.
pub(super) fn canonical_parent_with_missing(path: &Path) -> Result<PathBuf> {
    let leaf = path.file_name().context("missing workspace name")?;
    let parent = path.parent().context("missing workspace parent")?;
    let parent = canonical_with_missing_tail(parent)?;
    Ok(parent.join(leaf))
}

/// Whether a Git worktree path refers to a recorded workspace path.
pub(super) fn matches_recorded_worktree_path(recorded: &Path, candidate: &Path) -> Result<bool> {
    let canonical_recorded = canonical_parent_with_missing(recorded)?;
    Ok(candidate == canonical_recorded || candidate == recorded)
}

/// Whether `path` is a real, non-redirected directory; `false` when missing.
pub(super) fn is_real_directory(path: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("inspect repository directory"),
    };
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "repository path is not a directory or was replaced by a symlink"
    );
    ensure!(
        fs::canonicalize(path)? == path,
        "repository path was redirected; refusing deletion"
    );
    Ok(true)
}

/// Identity of a real, non-redirected directory; `None` when missing.
pub(super) fn real_directory_identity(path: &Path) -> Result<Option<String>> {
    is_real_directory(path)?
        .then(|| directory_identity(path))
        .transpose()
}

#[cfg(test)]
mod tests;
