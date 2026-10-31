//! Detached checkouts of one commit for read-only work beside a workspace,
//! such as a review. Worktrunk's worktrees are always on a branch, so these
//! are plain Git worktrees that no workspace owns.
use std::path::Path;

use anyhow::{Context, Result};

/// Check out `commit` at `path`, a new linked worktree of `repo` on no branch.
/// No hooks run, so a repository's `post-checkout` hook sets nothing up.
pub async fn add(repo: &Path, path: &Path, commit: &str) -> Result<()> {
    let mut command = super::isolated_command_without_submodules(repo);
    command
        .args(["worktree", "add", "--detach", "--"])
        .arg(path)
        .arg(commit);
    super::output(command)
        .await
        .with_context(|| format!("check out {commit} at {}", path.display()))?;
    Ok(())
}

/// Remove the detached checkout at `path`, whatever it holds, and forget it
/// in its repository. A checkout whose repository is gone is deleted.
pub async fn remove(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
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

#[cfg(test)]
mod tests;
