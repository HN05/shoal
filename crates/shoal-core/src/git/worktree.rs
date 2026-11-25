//! The linked worktrees of workspaces on a branch: creating one, and
//! removing any workspace's worktree, with its branch when the caller says
//! so, or what is left of one Git disowned. No hooks run.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use shoal_refs::BranchName;

use crate::{fsutil::off_runtime, tools::Tool};
use shoal_protocol::removal::{BranchOutcome, RemovalResult};

pub mod interrupted;
pub mod left;

/// Create a linked worktree of `repo` at `path` on `branch`: a new branch at
/// revision `base` when given, otherwise the existing branch, which Git
/// refuses when another worktree has it checked out.
pub async fn add(repo: &Path, path: &Path, branch: &BranchName, base: Option<&str>) -> Result<()> {
    let context = || format!("create a worktree on {branch} at {}", path.display());
    if let Some(base) = base {
        (super::branch::create(repo, branch, base).await).with_context(context)?;
    }
    (add_on_branch(repo, path, branch).await).with_context(context)
}

/// Check out the existing `branch` in a new linked worktree of `repo` at
/// `path`.
async fn add_on_branch(repo: &Path, path: &Path, branch: &BranchName) -> Result<()> {
    super::worktree_add::run(|| {
        let mut command = super::isolated_command_without_submodules(repo);
        command
            .args(["worktree", "add", "--"])
            .arg(path)
            .arg(branch.as_str());
        command
    })
    .await
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
    for worktree in super::listed_worktrees(repo, super::run_isolated).await? {
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
    // Without a marker, a removal stopped partway leaves its deletions as
    // removals did before markers, rather than not running at all.
    let admin_dir = mark_removal(path, link.as_deref())
        .await
        .unwrap_or_else(|error| {
            eprintln!("warning: removal of {} unmarked: {error:#}", path.display());
            None
        });
    if let Err(Unfinished { error, stopped }) = run_removal(command).await {
        let failed = || format!("remove the worktree at {}: {error:#}", path.display());
        // Once Git starts deleting files it forgets the worktree, whatever
        // it could not delete, so the rest goes as a disowned worktree's.
        if listed(repo, path).await.with_context(failed)?.is_some() {
            return Err(unfinished_removal(path, admin_dir.as_deref(), stopped, error).await);
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

/// Record that a removal begins deleting the worktree at `path`, whose
/// `.git` link reads `link`, in the admin directory it names, which this
/// returns; nothing for a worktree without one, whose files are gone.
async fn mark_removal(path: &Path, link: Option<&str>) -> Result<Option<PathBuf>> {
    let Some((text, admin_dir)) =
        link.and_then(|text| Some((text, interrupted::admin_dir(path, text)?)))
    else {
        return Ok(None);
    };
    interrupted::mark(&admin_dir, path, text.to_owned()).await?;
    Ok(Some(admin_dir))
}

/// A `worktree remove` that did not succeed.
struct Unfinished {
    error: anyhow::Error,
    /// It was stopped, by its timeout or a signal, rather than exiting.
    stopped: bool,
}

/// Run the `worktree remove` `command`.
async fn run_removal(command: tokio::process::Command) -> Result<(), Unfinished> {
    let ran = super::bounded(command).capture().await;
    let stopped = match &ran {
        Err(error) => error.kind() == std::io::ErrorKind::TimedOut,
        Ok(output) => output.status.code().is_none(),
    };
    crate::subprocess::checked_output(Tool::Git.program(), ran)
        .map(drop)
        .map_err(|error| Unfinished { error, stopped })
}

/// The error of a `worktree remove` that failed with `error` while Git
/// still lists the worktree at `path`, whose admin directory is
/// `admin_dir`. Git refuses before it deletes anything and forgets the
/// worktree once it has, so only a run that was `stopped` can have deleted
/// files: the worktree's own come back, as the next removal would put them
/// back too.
async fn unfinished_removal(
    path: &Path,
    admin_dir: Option<&Path>,
    stopped: bool,
    error: anyhow::Error,
) -> anyhow::Error {
    let failed = format!("remove the worktree at {}", path.display());
    let Some(admin_dir) = admin_dir else {
        return error.context(failed);
    };
    if !stopped {
        return match interrupted::forget(admin_dir).await {
            Ok(()) => error.context(failed),
            Err(forget) => error.context(format!("{failed}; {forget:#}")),
        };
    }
    match interrupted::restore(path, admin_dir).await {
        Ok(_) => error.context(format!(
            "{failed}: it stopped partway, so its tracked files were checked out again; removing it again continues with what is left"
        )),
        Err(restore) => error
            .context(format!(
                "checking its tracked files out again failed: {restore:#}; removing it again puts them back first"
            ))
            .context(interrupted::Unrestored(path.to_owned())),
    }
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

/// What a directory at a worktree's path holds once Git disowned the
/// worktree, deleting its admin directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leftover {
    /// A `.git` link still naming that admin directory, which proves the
    /// directory is the worktree's, as `worktree remove` leaves it when it
    /// cannot delete every file.
    Linked,
    /// No `.git`, as a removal stopped after deleting the link leaves it
    /// once a `worktree prune` deleted the admin directory no link named
    /// any longer; only the directory's own identity tells it from one that
    /// replaced the worktree.
    Unlinked,
}

/// What is left at `path` of the linked worktree whose admin directory was
/// `admin_dir`, when Git disowned it: that directory is gone while a
/// directory stays at `path` with no `.git` naming another.
pub(crate) async fn disowned(path: &Path, admin_dir: &Path) -> Result<Option<Leftover>> {
    if !off_runtime::is_dir(path).await? || off_runtime::try_exists(admin_dir).await? {
        return Ok(None);
    }
    let linked = {
        let path = path.to_owned();
        off_runtime::run(move || linked_admin_dir(&path)).await
    };
    Ok(match linked {
        Ok(None) => Some(Leftover::Unlinked),
        Ok(Some(named)) if resolved(&named).await == resolved(admin_dir).await => {
            Some(Leftover::Linked)
        }
        // A link naming another admin directory, a `.git` directory or a
        // link that cannot be read is another checkout's.
        Ok(Some(_)) | Err(_) => None,
    })
}

/// Whether the directory at `path` is what a `worktree add` of `repo` whose
/// checkout failed left there when it could not delete every file: its
/// `.git` link names an admin directory among `repo`'s worktrees that Git
/// deleted.
pub(crate) async fn left_by_failed_add(path: &Path, repo: &Path) -> Result<bool> {
    let Some(named) = vanished_admin_dir(path).await? else {
        return Ok(false);
    };
    let worktrees = super::common_dir(repo).await?.join("worktrees");
    Ok(match named.parent() {
        Some(parent) => resolved(parent).await == resolved(&worktrees).await,
        None => false,
    })
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
    interrupted::admin_dir(path, &contents)
        .with_context(|| format!("{} links no admin directory", link.display()))
        .map(Some)
}

/// The admin directory the `.git` link of the linked worktree at `path`
/// names, when that directory records `path` as its worktree; read before
/// a removal that can delete the link, so that only this worktree's admin
/// directory goes with it. Resolved, since a relative link names it
/// through the worktree's directory, which the removal deletes.
pub(crate) async fn own_admin_dir(path: &Path) -> Result<Option<PathBuf>> {
    let linked = {
        let path = path.to_owned();
        off_runtime::run(move || linked_admin_dir(&path)).await
    };
    // No link, or one that cannot be read, names no admin directory.
    let Ok(Some(admin_dir)) = linked else {
        return Ok(None);
    };
    let admin_dir = resolved(&admin_dir).await;
    Ok(registered_at(&admin_dir, path).await?.then_some(admin_dir))
}

/// The admin directory, among those of the repository whose shared Git
/// directory is `common_dir`, that records the directory at `path` as the
/// worktree an add finished; `None` when none does, as when Git never
/// created one there. It reads Git's record, not the worktree's `.git`
/// link, which an add cut short may not have written yet.
pub async fn admin_dir_at(common_dir: &Path, path: &Path) -> Result<Option<PathBuf>> {
    Ok(match registered_admin_dir(common_dir, path).await? {
        Some(admin_dir) if !adding(&admin_dir).await? => Some(admin_dir),
        _ => None,
    })
}

/// Delete what a `git worktree add` at `path` that was cut short left, its
/// admin directory among those of the repository whose shared Git
/// directory is `common_dir` and the directory, while that holds nothing
/// but the `.git` link the add wrote; whether there was any. A directory
/// with files in it is refused: its checkout may have begun, and anyone
/// may have worked there since.
pub async fn discard_unfinished_add(common_dir: &Path, path: &Path) -> Result<bool> {
    let Some(admin_dir) = registered_admin_dir(common_dir, path).await? else {
        return Ok(false);
    };
    if !adding(&admin_dir).await? {
        return Ok(false);
    }
    ensure!(
        holds_only_link(path).await?,
        "a worktree add that was cut short left files in {}; move or delete them",
        path.display()
    );
    delete_with_admin_dir(path, Some(admin_dir)).await?;
    Ok(true)
}

/// Whether the directory at `path` is missing or holds nothing but a
/// worktree's `.git` link.
async fn holds_only_link(path: &Path) -> Result<bool> {
    let path = path.to_owned();
    off_runtime::run(move || match std::fs::read_dir(&path) {
        Ok(entries) => {
            for entry in entries {
                if entry?.file_name() != ".git" {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    })
    .await
}

/// The branch the worktree whose admin directory is `admin_dir` has
/// checked out, as its `HEAD` names it; `None` at a detached HEAD.
pub async fn admin_dir_branch(admin_dir: &Path) -> Result<Option<BranchName>> {
    let head = admin_dir.join("HEAD");
    let text = off_runtime::run(move || {
        std::fs::read_to_string(&head).with_context(|| format!("read {}", head.display()))
    })
    .await?;
    Ok((text.trim_end().strip_prefix("ref: "))
        .and_then(super::strip_local)
        .map(BranchName::from))
}

/// The admin directory among those of the repository whose shared Git
/// directory is `common_dir` that records the directory at `path`, whether
/// its add finished or not.
async fn registered_admin_dir(common_dir: &Path, path: &Path) -> Result<Option<PathBuf>> {
    let worktrees = common_dir.join("worktrees");
    let entries = off_runtime::run(move || match std::fs::read_dir(&worktrees) {
        Ok(entries) => (entries.map(|entry| Ok(entry?.path())))
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("read {}", worktrees.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("read {}", worktrees.display())),
    })
    .await?;
    for admin_dir in entries {
        if registered_at(&admin_dir, path).await? {
            return Ok(Some(admin_dir));
        }
    }
    Ok(None)
}

/// Whether the add that made the admin directory `admin_dir` is still
/// under way or was cut short: Git keeps it locked until its checkout
/// finishes, and Shoal never adds a worktree that stays locked.
async fn adding(admin_dir: &Path) -> Result<bool> {
    off_runtime::try_exists(&admin_dir.join("locked")).await
}

/// Whether the admin directory `admin_dir` records the directory at `path`
/// as its worktree in its `gitdir` file, relative to it when
/// `worktree.useRelativePaths` is set.
async fn registered_at(admin_dir: &Path, path: &Path) -> Result<bool> {
    let gitdir = admin_dir.join("gitdir");
    let text = off_runtime::run(move || {
        crate::fsutil::read_optional(&gitdir).with_context(|| format!("read {}", gitdir.display()))
    })
    .await?;
    let Some(text) = text else {
        return Ok(false);
    };
    let named = admin_dir.join(text.trim_end_matches('\n'));
    Ok(resolved(&named).await == resolved(&path.join(".git")).await)
}

/// Delete the directory of the linked worktree at `path`, whatever it
/// holds, then its admin directory `admin_dir`, which [`own_admin_dir`]
/// read, while that still records `path`. Unlike `git worktree prune`, this
/// takes no other worktree's admin directory, such as that of a workspace
/// whose removal stopped after Git deleted its link, which the next removal
/// reads to put the link back.
pub(crate) async fn delete_with_admin_dir(path: &Path, admin_dir: Option<PathBuf>) -> Result<()> {
    delete_disowned(path).await?;
    let Some(admin_dir) = admin_dir else {
        return Ok(());
    };
    // A failed `worktree remove` may already have deleted it, and another
    // worktree created since may have taken its name; while it records
    // `path`, it is still this one's, and Git gives no new worktree a name
    // in use.
    if !registered_at(&admin_dir, path).await? {
        return Ok(());
    }
    delete_admin_dir(admin_dir).await
}

/// Delete the admin directory `admin_dir`, unless it is gone.
async fn delete_admin_dir(admin_dir: PathBuf) -> Result<()> {
    off_runtime::run(move || match std::fs::remove_dir_all(&admin_dir) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("remove {}", admin_dir.display()))
        }
        _ => Ok(()),
    })
    .await
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
    let modules = super::git_path(path, "modules").await?;
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
    let checked_out = super::listed_checkout_of(repo, branch, super::run_isolated).await?;
    Ok(match checked_out {
        Some(_) => BranchOutcome::RetainedCheckedOut,
        None => BranchOutcome::RetainedFailed,
    })
}

#[cfg(test)]
mod tests;
