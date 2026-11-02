//! Detached checkouts of one commit: the worktrees of workspaces at a
//! commit, and read-only checkouts beside a workspace, such as a review's,
//! that no workspace owns. Worktrunk's worktrees are always on a branch, so
//! these are plain Git worktrees.
use std::path::Path;

use anyhow::{Context, Result};

/// Check out `commit` at `path`, a new linked worktree of `repo` on no branch,
/// whose files and directories nobody may write. No hooks run, so a
/// repository's `post-checkout` hook sets nothing up. The caller owns
/// `path`, so a registration a killed checkout left there, locked while it
/// was being created, is replaced.
pub async fn add(repo: &Path, path: &Path, commit: &str) -> Result<()> {
    worktree_add(repo, path, commit, &["--force", "--force"]).await?;
    set_writable(path, false).with_context(|| format!("make {} read-only", path.display()))
}

/// Check out `commit` at `path`, a new linked worktree of `repo` on no
/// branch, for a workspace at a commit. No hooks run, as for the worktrees
/// Worktrunk creates on a branch.
pub async fn add_workspace(repo: &Path, path: &Path, commit: &str) -> Result<()> {
    worktree_add(repo, path, commit, &[]).await
}

async fn worktree_add(repo: &Path, path: &Path, commit: &str, options: &[&str]) -> Result<()> {
    let mut command = super::isolated_command_without_submodules(repo);
    command
        .args(["worktree", "add", "--detach"])
        .args(options)
        .arg("--")
        .arg(path)
        .arg(commit);
    super::output(command)
        .await
        .with_context(|| format!("check out {commit} at {}", path.display()))
        .map(drop)
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
