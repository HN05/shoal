//! Inspecting what removing a workspace would discard.
use anyhow::Result;

pub(crate) use crate::client::removal::{
    BranchOutcome, NOTHING, RemovalCheck, RemovalResult, UnsavedWork,
};
use crate::{fsutil::off_runtime, git, model::Workspace, process};

#[cfg(test)]
mod tests;

/// A queued restore of an agent stopped between turns with every process,
/// waiting for an update that has not arrived: nothing runs for it until
/// then, so it keeps the workspace from no cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IdleRestore {
    pub execution: String,
    /// The wrapper waiting to restore the agent, which may use the
    /// workspace directory; none when the daemon starts the restore.
    pub wrapper: Option<u32>,
}

/// A [`RemovalCheck`] with the daemon state that keeps the removal decision
/// consistent with the inspection, which the protocol leaves out.
#[derive(Debug)]
pub(crate) struct Inspection {
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
    /// The processes `users` finds in the worktree, besides the wrappers
    /// waiting to restore idle agents.
    pub async fn load_directory_processes(
        &mut self,
        users: &process::DirectoryUsers,
    ) -> Result<()> {
        self.processes = users
            .in_directory_except(&self.workspace.path, &wrappers(&self.idle_restores))
            .await?;
        Ok(())
    }
}

/// Fill in the changed files `check` lists.
pub(crate) async fn load_changed_files(check: &mut RemovalCheck) -> Result<()> {
    let status = git::status::changed_files(&check.workspace.path).await?;
    (check.changed_files, check.changed_files_omitted) = changed_files_preview(&status);
    Ok(())
}

/// The wrappers waiting to restore the idle agents, which may use the
/// workspace directory without using the workspace.
pub(crate) fn wrappers(restores: &[IdleRestore]) -> Vec<u32> {
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
/// safe. `running_commands` leaves out the `idle_restores`.
pub(crate) async fn inspect(
    workspace: Workspace,
    running_commands: usize,
    idle_restores: Vec<IdleRestore>,
    policy: InspectionPolicy,
    default_branch: Option<&str>,
) -> Result<Inspection> {
    let mut inspection = Inspection {
        check: RemovalCheck {
            workspace,
            running_commands,
            children: vec![],
            disposable_children: vec![],
            processes: vec![],
            dirty: false,
            starting_files: false,
            starting_commits: false,
            changed_files: vec![],
            changed_files_omitted: 0,
            unsaved_files_archived: false,
            unpushed_commits: 0,
            branch: None,
            head: None,
            matches_default_branch: false,
            integrated: false,
        },
        default_branch: default_branch.map(str::to_owned),
        idle_restores,
    };
    if !off_runtime::exists(&inspection.workspace.path).await? {
        return Ok(inspection);
    }
    inspect_git(&mut inspection.check, default_branch).await?;
    if policy == InspectionPolicy::IncludeDirectoryProcesses {
        inspection
            .load_directory_processes(&process::DirectoryUsers::default())
            .await?;
    }
    Ok(inspection)
}

/// Fill in what Git says removing the existing worktree would discard.
/// Nothing in a disposable workspace is worth keeping, so nothing is
/// inspected for unsaved work; which branch it is on, which removal may
/// delete, still is.
async fn inspect_git(check: &mut RemovalCheck, default_branch: Option<&str>) -> Result<()> {
    if check.workspace.disposable {
        check.branch = checked_out_branch(&check.workspace.path).await?;
        return Ok(());
    }
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
        Some(name) => git::status::default_refs(path, name).await?,
        None => Vec::new(),
    };
    check.matches_default_branch = git::status::retains(path, &default_refs, "HEAD", &tree).await?;
    // Merged changes are on the default branch even when their commits are not.
    if !check.matches_default_branch {
        let kept = kept_refs(&check.workspace, check.branch.is_none(), &default_refs);
        check.unpushed_commits = git::status::unretained_commits(path, "HEAD", &kept).await?;
        check.integrated = check.unpushed_commits > 0
            && start_branch_retains_head(&check.workspace, default_branch, &tree).await?;
        check.starting_commits =
            check.unpushed_commits > 0 && only_starting_commits(&check.workspace, &kept).await?;
    }
    Ok(())
}

/// Whether the workspace is a copy still at its starting HEAD with the
/// index it started with and the files of its starting checkpoint, which
/// are not its own work. A Git operation under way is the copy's own, and
/// leaves an index that cannot have been its starting one.
pub(crate) async fn holds_starting_files(workspace: &Workspace) -> Result<bool> {
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
            && git::checkpoint::matches(path, &copied.commit).await?,
    )
}

/// The branch the worktree at `path` is on; `None` when detached.
async fn checked_out_branch(path: &std::path::Path) -> Result<Option<String>> {
    let branch = git::current_branch(path).await?;
    let branch = branch.trim_end_matches('\n');
    Ok((!branch.is_empty()).then(|| branch.to_owned()))
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
    let start = workspace.at.as_ref().filter(|_| detached);
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
    kept.push(&copied.head);
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
    let Some(reference) = git::status::local_branch(&workspace.path, name).await? else {
        return Ok(false);
    };
    git::status::retains(&workspace.path, &[reference], "HEAD", tree).await
}

/// The branch the workspace builds on: its base workspace's branch,
/// otherwise the local branch its recorded base names; never its own.
fn start_branch(workspace: &Workspace) -> Option<&str> {
    let branch = match &workspace.base_workspace {
        Some(base) => base.branch.as_deref()?,
        None => workspace
            .base_ref
            .as_deref()?
            .strip_prefix(git::LOCAL_REFS)?,
    };
    (workspace.branch.as_deref() != Some(branch)).then_some(branch)
}
