//! The linked worktrees of workspaces on a branch: creating one, and
//! removing any workspace's worktree, with its branch when the caller says
//! so, or what is left of one Git disowned. No hooks run.
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
    let Some(worktree) = listed(repo, path).await? else {
        anyhow::bail!("Git lists no worktree at {}", path.display())
    };
    Ok(worktree
        .branch
        .as_deref()
        .and_then(super::strip_local)
        .map(BranchName::from))
}

/// The worktree Git lists at `path`, if any.
async fn listed(repo: &Path, path: &Path) -> Result<Option<super::Worktree>> {
    let path = resolved(path).await;
    for worktree in super::worktrees(repo, super::run_isolated).await? {
        if resolved(&worktree.path).await == path {
            return Ok(Some(worktree));
        }
    }
    Ok(None)
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
    let link = link_text(path).await?;
    if let Err(error) = super::output(command).await {
        let failed = || format!("remove the worktree at {}: {error:#}", path.display());
        // Once Git starts deleting files it forgets the worktree, whatever
        // it could not delete, so the rest goes as a disowned worktree's.
        if listed(repo, path).await.with_context(failed)?.is_some() {
            return Err(error.context(format!("remove the worktree at {}", path.display())));
        }
        finish_deletion(path, link).await.with_context(failed)?;
    }
    ensure!(
        !off_runtime::exists(path).await?,
        "Git left {} after removing its worktree",
        path.display()
    );
    Ok(())
}

/// The text of the `.git` link of the worktree at `path`, if it has one.
async fn link_text(path: &Path) -> Result<Option<String>> {
    let link = path.join(".git");
    off_runtime::run(move || Ok(crate::fsutil::read_optional(&link)?)).await
}

/// Delete what Git left of the worktree at `path` once it stopped listing
/// it, first putting back its `.git` link, whose text was `link` before Git
/// may have deleted it, so that a deletion cut short again leaves the proof
/// that the directory is the worktree Git disowned.
async fn finish_deletion(path: &Path, link: Option<String>) -> Result<()> {
    if let Some(text) = link {
        restore_link(path, text).await?;
    }
    delete_disowned(path).await
}

/// Write the `.git` link of the worktree at `path` back with `text`
/// unless it is there, or the directory is gone.
async fn restore_link(path: &Path, text: String) -> Result<()> {
    let link = path.join(".git");
    off_runtime::run(move || {
        if link.parent().is_some_and(Path::is_dir) && !link.try_exists()? {
            std::fs::write(&link, text).with_context(|| format!("restore {}", link.display()))?;
        }
        Ok(())
    })
    .await
}

/// Whether Git disowned the linked worktree at `path`, whose admin
/// directory was `admin_dir`: that directory is gone, as `worktree remove`
/// leaves it when it cannot delete every file, while the worktree's
/// directory stays with a `.git` link still naming it.
pub(crate) async fn disowned(path: &Path, admin_dir: &Path) -> Result<bool> {
    Ok(match vanished_admin_dir(path).await? {
        Some(named) => resolved(&named).await == resolved(admin_dir).await,
        None => false,
    })
}

/// Whether the directory at `path` is what a `worktree add` whose checkout
/// failed left there when it could not delete every file: its `.git` link
/// names an admin directory Git deleted.
pub(crate) async fn left_by_failed_add(path: &Path) -> Result<bool> {
    Ok(vanished_admin_dir(path).await?.is_some())
}

/// The admin directory the `.git` link of the directory at `path` names,
/// when the directory stays while that admin directory is gone.
async fn vanished_admin_dir(path: &Path) -> Result<Option<PathBuf>> {
    if !off_runtime::is_dir(path).await? {
        return Ok(None);
    }
    let linked = {
        let path = path.to_owned();
        off_runtime::run(move || linked_admin_dir(&path)).await
    };
    match linked {
        Ok(Some(named)) if !off_runtime::try_exists(&named).await? => Ok(Some(named)),
        // No link, a `.git` directory or a link that cannot be read proves
        // nothing about whose directory it is.
        Ok(_) | Err(_) => Ok(None),
    }
}

/// The admin directory the `.git` link of the worktree at `path` names;
/// `None` when it has none. Git writes a relative link when
/// `worktree.useRelativePaths` is set.
fn linked_admin_dir(path: &Path) -> Result<Option<PathBuf>> {
    let link = path.join(".git");
    let Some(contents) = crate::fsutil::read_optional(&link)? else {
        return Ok(None);
    };
    let named = (contents.strip_prefix("gitdir: "))
        .with_context(|| format!("{} links no admin directory", link.display()))?;
    Ok(Some(path.join(named.trim_end_matches('\n'))))
}

/// Delete the directory of the worktree at `path` that Git disowned,
/// whatever it holds, keeping its `.git` link until nothing else is left.
pub async fn delete_disowned(path: &Path) -> Result<()> {
    let path = path.to_owned();
    off_runtime::run(move || {
        delete_link_last(&path).with_context(|| format!("remove {}", path.display()))
    })
    .await
}

/// Delete the directory `path` and what it holds, its `.git` link last; a
/// deletion that fails, as when something writes into the directory
/// meanwhile, leaves the link.
fn delete_link_last(path: &Path) -> std::io::Result<()> {
    if !path.try_exists()? {
        return Ok(());
    }
    super::detached::set_writable(path, true)?;
    let link = path.join(".git");
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        match entry.file_type()? {
            _ if entry.path() == link => {}
            kind if kind.is_dir() => std::fs::remove_dir_all(entry.path())?,
            _ => std::fs::remove_file(entry.path())?,
        }
    }
    let text = crate::fsutil::read_optional(&link)?;
    if text.is_some() {
        std::fs::remove_file(&link)?;
    }
    if let Err(error) = std::fs::remove_dir(path) {
        if let Some(text) = text {
            std::fs::write(&link, text)?;
        }
        return Err(error);
    }
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
