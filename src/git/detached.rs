//! Detached checkouts of one commit for read-only work beside a workspace,
//! such as a review. Worktrunk's worktrees are always on a branch, so these
//! are plain Git worktrees that no workspace owns, and their files are
//! read-only until they are removed.
use std::path::Path;

use anyhow::{Context, Result};

/// Check out `commit` at `path`, a new linked worktree of `repo` on no branch,
/// whose files and directories nobody may write. No hooks run, so a
/// repository's `post-checkout` hook sets nothing up.
pub async fn add(repo: &Path, path: &Path, commit: &str) -> Result<()> {
    let mut command = super::isolated_command_without_submodules(repo);
    command
        .args(["worktree", "add", "--detach", "--"])
        .arg(path)
        .arg(commit);
    super::output(command)
        .await
        .with_context(|| format!("check out {commit} at {}", path.display()))?;
    set_writable(path, false).with_context(|| format!("make {} read-only", path.display()))
}

/// Remove the detached checkout at `path`, whatever it holds, and forget it
/// in its repository. A checkout whose repository is gone is deleted.
pub async fn remove(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    set_writable(path, true).with_context(|| format!("make {} writable", path.display()))?;
    if let Ok(common) = super::common_dir(path).await {
        let mut command = super::isolated_command(&common);
        command
            .args(["worktree", "remove", "--force", "--"])
            .arg(path);
        if super::output(command).await.is_ok() {
            return Ok(());
        }
        std::fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
        return super::run_isolated(&common, &["worktree", "prune"])
            .await
            .map(drop);
    }
    std::fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))
}

/// Give or take the write permission on every file and directory under
/// `path`, without following symbolic links: the owner's when giving it,
/// everyone's when taking it.
fn set_writable(path: &Path, writable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            set_writable(&entry?.path(), writable)?;
        }
    }
    let mode = metadata.permissions().mode();
    let mode = match writable {
        true => mode | 0o200,
        false => mode & !0o222,
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests;
