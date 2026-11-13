//! The linked worktrees of workspaces on a branch: creating one, and
//! removing any workspace's worktree, with its branch when the caller says
//! so. No hooks run.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use shoal_refs::BranchName;

use crate::{
    fsutil::off_runtime,
    removal::{BranchOutcome, RemovalResult},
};

/// Create a linked worktree of `repo` at `path` on `branch`: a new branch at
/// revision `base` when given, otherwise the existing branch, which Git
/// refuses when another worktree has it checked out.
pub async fn add(repo: &Path, path: &Path, branch: &BranchName, base: Option<&str>) -> Result<()> {
    let mut command = super::isolated_command_without_submodules(repo);
    command.args(["worktree", "add"]);
    match base {
        Some(base) => command
            .args(["-b", branch.as_str(), "--"])
            .arg(path)
            .arg(base),
        None => command.arg("--").arg(path).arg(branch.as_str()),
    };
    super::output(command)
        .await
        .with_context(|| format!("create a worktree on {branch} at {}", path.display()))
        .map(drop)
}

/// Whether removal may delete files Git would keep the worktree for.
#[derive(Debug, Clone, Copy)]
pub enum FileRemoval {
    /// Refuse a worktree with changed or untracked files.
    CleanOnly,
    Force,
}

#[derive(Debug, Clone, Copy)]
pub enum BranchRemoval {
    Keep,
    /// Delete the branch the worktree is on, merged or not.
    Delete,
}

/// Remove the linked worktree of `repo` at `path`, which Git still lists
/// though its directory may be gone, then the branch it was on when `branch`
/// says so. The branch stays when another worktree has it checked out or Git
/// cannot delete it, which the outcome reports.
pub async fn remove(
    repo: &Path,
    path: &Path,
    files: FileRemoval,
    branch: BranchRemoval,
) -> Result<RemovalResult> {
    let checked_out = registered_branch(repo, path).await?;
    remove_worktree(repo, path, files).await?;
    let branch_outcome = match (&checked_out, branch) {
        (Some(name), BranchRemoval::Delete) => delete_branch(repo, name).await?,
        _ => BranchOutcome::NotAttempted,
    };
    Ok(RemovalResult {
        removed: true,
        branch: checked_out,
        branch_outcome,
        hook_error: None,
        children: Vec::new(),
    })
}

/// The branch Git lists the worktree at `path` on; `None` when detached.
async fn registered_branch(repo: &Path, path: &Path) -> Result<Option<BranchName>> {
    let path = resolved(path).await;
    for worktree in super::worktrees(repo, super::run_isolated).await? {
        if resolved(&worktree.path).await == path {
            return Ok(worktree
                .branch
                .as_deref()
                .and_then(super::strip_local)
                .map(BranchName::from));
        }
    }
    anyhow::bail!("Git lists no worktree at {}", path.display())
}

/// `path` with symbolic links resolved as far as it exists.
async fn resolved(path: &Path) -> PathBuf {
    if let Ok(path) = off_runtime::canonicalize(path).await {
        return path;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => Box::pin(resolved(parent)).await.join(name),
        _ => path.to_owned(),
    }
}

async fn remove_worktree(repo: &Path, path: &Path, files: FileRemoval) -> Result<()> {
    let mut command = super::isolated_command(repo);
    command.args(["worktree", "remove"]);
    if matches!(files, FileRemoval::Force) || needs_structural_force(path).await? {
        command.arg("--force");
    }
    command.arg("--").arg(path);
    super::output(command)
        .await
        .with_context(|| format!("remove the worktree at {}", path.display()))?;
    ensure!(
        !off_runtime::exists(path).await?,
        "Git left {} after removing its worktree",
        path.display()
    );
    Ok(())
}

/// Whether the clean worktree at `path` has submodules, which Git refuses
/// to remove without `--force`: checked out, or deinitialized while their
/// repositories stay in its own Git directory. The force also skips Git's
/// own check for changes, so this makes it first and fails on any.
async fn needs_structural_force(path: &Path) -> Result<bool> {
    if !off_runtime::exists(path).await? || !has_submodules(path).await? {
        return Ok(false);
    }
    let changes = super::run_isolated(
        path,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ],
    )
    .await?;
    ensure!(
        changes.is_empty(),
        "the worktree at {} has changed or untracked files",
        path.display()
    );
    Ok(true)
}

/// Git's own test: the worktree's Git directory holds `modules`, or a
/// submodule is checked out.
async fn has_submodules(path: &Path) -> Result<bool> {
    let modules = super::copy::admin_path(path, "modules").await?;
    Ok(off_runtime::exists(&modules).await?
        || !super::copy::initialized_submodules(path).await?.is_empty())
}

/// Delete `branch`, saying why it stayed when Git refused.
async fn delete_branch(repo: &Path, branch: &BranchName) -> Result<BranchOutcome> {
    if super::run_isolated(repo, &["branch", "-D", "--", branch.as_str()])
        .await
        .is_ok()
    {
        return Ok(BranchOutcome::Deleted);
    }
    let checked_out = super::checkout_of(repo, branch, super::run_isolated).await?;
    Ok(match checked_out {
        Some(_) => BranchOutcome::RetainedCheckedOut,
        None => BranchOutcome::RetainedFailed,
    })
}

#[cfg(test)]
mod tests;
