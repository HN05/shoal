//! Inspecting what removing a workspace would discard.
use anyhow::{Context, Result};
use shoal_ids::execution_ids::ExecutionId;
use shoal_refs::{BranchName, CommitId};

use crate::{
    fsutil::off_runtime,
    git::{
        self,
        default_branch::DefaultBranch,
        worktree::{Leftover, left::LeftFiles},
    },
    model::{Head, Workspace},
    process,
};
pub use shoal_protocol::removal::{
    BranchOutcome, NOTHING, RemovalCheck, RemovalResult, SpaceRemovalCheck, UnsavedWork,
};
use std::path::Path;

#[cfg(test)]
mod tests;

/// A queued restore of an agent stopped between turns with every process,
/// waiting for an update that has not arrived: nothing runs for it until
/// then, so it keeps the workspace from no cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleRestore {
    pub execution: ExecutionId,
    /// The wrapper waiting to restore the agent, which may use the
    /// workspace directory; none when the daemon starts the restore.
    pub wrapper: Option<u32>,
}

/// A [`RemovalCheck`] with the daemon state that keeps the removal decision
/// consistent with the inspection, which the protocol leaves out.
#[derive(Debug)]
pub struct Inspection {
    pub check: RemovalCheck,
    /// The default branch the inspection used, when it could be resolved.
    pub default_branch: Option<String>,
    /// The restores of idle agents, which `running_commands` and
    /// `processes` leave out.
    pub idle_restores: Vec<IdleRestore>,
}

impl std::ops::Deref for Inspection {
    type Target = RemovalCheck;

    fn deref(&self) -> &RemovalCheck {
        &self.check
    }
}

impl std::ops::DerefMut for Inspection {
    fn deref_mut(&mut self) -> &mut RemovalCheck {
        &mut self.check
    }
}

impl Inspection {
    /// The processes `users` finds active in the worktree, besides the
    /// wrappers waiting to restore idle agents.
    pub async fn load_directory_processes(
        &mut self,
        users: &process::DirectoryUsers,
    ) -> Result<()> {
        self.processes = users
            .activity_except(&self.workspace.path, &wrappers(&self.idle_restores))
            .await?;
        Ok(())
    }
}

/// Fill in the changed files `check` lists; `repo` is the checkout of the
/// workspace's repository.
pub async fn load_changed_files(check: &mut RemovalCheck, repo: &Path) -> Result<()> {
    let status = match check.disowned {
        true => read_left(&check.workspace, repo).await?.status().await?,
        false => git::status::changed_files(&check.workspace.path).await?,
    };
    (check.changed_files, check.changed_files_omitted) = changed_files_preview(&status);
    Ok(())
}

/// The wrappers waiting to restore the idle agents, which may use the
/// workspace directory without using the workspace.
pub fn wrappers(restores: &[IdleRestore]) -> Vec<u32> {
    restores
        .iter()
        .filter_map(|restore| restore.wrapper)
        .collect()
}

fn changed_files_preview(status: &str) -> (Vec<String>, usize) {
    // Leave room for the rest of RemovalCheck in the 64 KiB protocol frame.
    const MAX_BYTES: usize = 16 * 1024;
    const MAX_ENTRIES: usize = 50;
    let mut files = Vec::new();
    let mut bytes = 0;
    let mut omitted = 0;
    for line in status.lines() {
        let encoded_len = serde_json::to_string(line)
            .expect("strings serialize")
            .len()
            + 1;
        if omitted > 0 || files.len() == MAX_ENTRIES || bytes + encoded_len > MAX_BYTES {
            omitted += 1;
        } else {
            bytes += encoded_len;
            files.push(line.to_owned());
        }
    }
    (files, omitted)
}

/// Whether a removal inspection includes processes using the directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectionPolicy {
    GitOnly,
    IncludeDirectoryProcesses,
}

/// Gather state for a removal decision without deciding whether removal is
/// safe. `running_commands` leaves out the `idle_restores`; `repo` is the
/// checkout of the workspace's repository.
pub async fn inspect(
    workspace: Workspace,
    running_commands: usize,
    idle_restores: Vec<IdleRestore>,
    policy: InspectionPolicy,
    (repo, default_branch): (&Path, Option<&DefaultBranch>),
) -> Result<Inspection> {
    let mut inspection = Inspection {
        check: RemovalCheck {
            workspace,
            running_commands,
            processes: vec![],
            dirty: false,
            starting_files: false,
            starting_commits: false,
            changed_files: vec![],
            changed_files_omitted: 0,
            unsaved_files_archived: false,
            disowned: false,
            unpushed_commits: 0,
            branch: None,
            head: None,
            matches_default_branch: false,
            integrated: false,
        },
        default_branch: default_branch.map(|default| default.name.to_string()),
        idle_restores,
    };
    if !off_runtime::exists(&inspection.workspace.path).await? {
        return Ok(inspection);
    }
    inspect_git(&mut inspection.check, repo, default_branch).await?;
    if policy == InspectionPolicy::IncludeDirectoryProcesses {
        inspection
            .load_directory_processes(&process::DirectoryUsers::default())
            .await?;
    }
    Ok(inspection)
}

/// Fill in what removing the existing worktree would discard. Nothing in a
/// disposable workspace is worth keeping, so nothing is inspected for
/// unsaved work; which branch it is on, which removal may delete, still is,
/// unless Git disowned it.
async fn inspect_git(
    check: &mut RemovalCheck,
    repo: &Path,
    default_branch: Option<&DefaultBranch>,
) -> Result<()> {
    check.disowned = disowned(&check.workspace, repo).await?.is_some();
    if check.workspace.disposable {
        if !check.disowned {
            check.branch = git::checked_out_branch(&check.workspace.path).await?;
        }
        return Ok(());
    }
    match check.disowned {
        true => inspect_left(check, repo).await,
        false => inspect_worktree(check, default_branch).await,
    }
}

/// Fill in what removing the files left of a worktree Git disowned would
/// discard: those that differ from the commit it was read against. Git
/// keeps no HEAD there to read a branch or commits of.
async fn inspect_left(check: &mut RemovalCheck, repo: &Path) -> Result<()> {
    let left = read_left(&check.workspace, repo).await?;
    check.dirty = !left.status().await?.is_empty();
    Ok(())
}

/// Fill in what Git says removing the worktree it knows would discard.
async fn inspect_worktree(
    check: &mut RemovalCheck,
    default_branch: Option<&DefaultBranch>,
) -> Result<()> {
    let path = &check.workspace.path;
    let git::HeadState {
        commit,
        tree,
        branch,
    } = git::head_state(path).await?;
    let changed = !git::status::porcelain(path).await?.is_empty();
    check.starting_files = changed && holds_starting_files(&check.workspace).await?;
    check.dirty = changed && !check.starting_files;
    check.branch = branch;
    check.head = Some(commit);
    // The default branch may be absent locally (workspaces created from another ref).
    let default_refs = match default_branch {
        Some(default) => git::status::default_refs(path, default).await?,
        None => Vec::new(),
    };
    check.matches_default_branch = git::status::retains(path, &default_refs, "HEAD", &tree).await?;
    // Merged changes are on the default branch even when their commits are not.
    if !check.matches_default_branch {
        let kept = kept_refs(&check.workspace, check.branch.is_none(), &default_refs);
        check.unpushed_commits = git::status::unretained_commits(path, "HEAD", &kept).await?;
        check.integrated = check.unpushed_commits > 0
            && start_branch_retains_head(
                &check.workspace,
                default_branch.map(|default| default.name.as_str()),
                &tree,
            )
            .await?;
        check.starting_commits =
            check.unpushed_commits > 0 && only_starting_commits(&check.workspace, &kept).await?;
    }
    Ok(())
}

/// What is left of the worktree of `workspace` when Git disowned it, so
/// that removal deletes it without Git, once a checkpoint keeps what is
/// worth keeping: as a removal Git could not finish leaves it, or, for a
/// disposable workspace whose checkout never succeeded and so recorded no
/// admin directory, as a checkout that failed leaves it. Nothing was at a
/// workspace's path when Shoal recorded it but the worktree it adopted, so
/// a `.git` link there naming an admin directory of its repository, checked
/// out at `repo`, is Git's for this workspace; a directory without one is
/// only once its identity proves it.
pub async fn disowned(workspace: &Workspace, repo: &Path) -> Result<Option<Leftover>> {
    match &workspace.git_dir {
        Some(admin_dir) => git::worktree::disowned(&workspace.path, admin_dir).await,
        None if workspace.disposable => {
            Ok(
                (git::worktree::left_by_failed_add(&workspace.path, repo).await?)
                    .then_some(Leftover::Linked),
            )
        }
        None => Ok(None),
    }
}

/// The files left of the worktree of `workspace`, which Git disowned, read
/// against the commit its branch is at, or for a workspace at a commit, the
/// one it started at: Git no longer records which the worktree had.
pub async fn read_left(workspace: &Workspace, repo: &Path) -> Result<LeftFiles> {
    let head: CommitId = match &workspace.head {
        Head::Branch(branch) => git::resolve_commit(repo, &git::local_ref(branch), git::run)
            .await
            .with_context(|| {
                format!("Git no longer knows the worktree, and its branch {branch} is gone")
            })?,
        Head::Commit(at) => at.commit.clone(),
    };
    LeftFiles::read(repo, &workspace.path, &head).await
}

/// Whether the workspace is a copy still at its starting HEAD with the
/// index it started with and the files of its starting checkpoint, which
/// are not its own work. A Git operation under way is the copy's own, and
/// leaves an index that cannot have been its starting one.
pub async fn holds_starting_files(workspace: &Workspace) -> Result<bool> {
    let Some(copied) = &workspace.copied_from else {
        return Ok(false);
    };
    let path = &workspace.path;
    if git::status::operation_in_progress(path).await? {
        return Ok(false);
    }
    Ok(
        git::resolve_commit(path, "HEAD", git::run).await? == copied.head
            && git::checkpoint::index_tree(path).await? == copied.index
            && git::checkpoint::matches(path, copied.commit.as_str()).await?,
    )
}

/// What keeps a workspace's commits besides the remotes: the local default
/// branch, and for a workspace at a commit that is still `detached` the
/// commit it started at, such as a fork PR's head or a local commit no
/// remote has. Once its worktree is on a branch, that branch's commits need
/// the usual proof.
fn kept_refs<'a>(
    workspace: &'a Workspace,
    detached: bool,
    default_refs: &'a [String],
) -> Vec<&'a str> {
    let start = workspace.at().filter(|_| detached);
    default_refs
        .first()
        .map(String::as_str)
        .into_iter()
        .chain(start.map(|at| at.commit.as_str()))
        .collect()
}

/// Whether the workspace is a copy whose commits that `kept` leaves out are
/// all ones it started with, which its starting checkpoint keeps.
async fn only_starting_commits(workspace: &Workspace, kept: &[&str]) -> Result<bool> {
    let Some(copied) = &workspace.copied_from else {
        return Ok(false);
    };
    let mut kept = kept.to_vec();
    kept.push(copied.head.as_str());
    Ok(git::status::unretained_commits(&workspace.path, "HEAD", &kept).await? == 0)
}

/// Whether the local branch the workspace builds on, unless it is the
/// default branch, has HEAD's changes.
async fn start_branch_retains_head(
    workspace: &Workspace,
    default_branch: Option<&str>,
    tree: &str,
) -> Result<bool> {
    let Some(name) = start_branch(workspace).filter(|name| Some(*name) != default_branch) else {
        return Ok(false);
    };
    let Some(reference) = git::status::local_branch(&workspace.path, &name.into()).await? else {
        return Ok(false);
    };
    git::status::retains(&workspace.path, &[reference], "HEAD", tree).await
}

/// The branch the workspace builds on: its base workspace's branch,
/// otherwise the local branch its recorded base names; never its own.
fn start_branch(workspace: &Workspace) -> Option<&str> {
    let branch = match &workspace.base_workspace {
        Some(base) => base.branch.as_ref().map(BranchName::as_str)?,
        None => workspace
            .base_ref
            .as_deref()?
            .strip_prefix(git::LOCAL_REFS)?,
    };
    (workspace.branch().map(BranchName::as_str) != Some(branch)).then_some(branch)
}
