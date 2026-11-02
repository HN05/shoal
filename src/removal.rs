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

impl RemovalCheck {
    pub async fn load_changed_files(&mut self) -> Result<()> {
        let status = git::status::changed_files(&self.workspace.path).await?;
        (self.changed_files, self.changed_files_omitted) = changed_files_preview(&status);
        Ok(())
    }

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
) -> Result<RemovalCheck> {
    let mut check = RemovalCheck {
        workspace,
        running_commands,
        idle_restores,
        children: vec![],
        disposable_children: vec![],
        processes: vec![],
        dirty: false,
        changed_files: vec![],
        changed_files_omitted: 0,
        unsaved_files_archived: false,
        unpushed_commits: 0,
        branch: None,
        head: None,
        matches_default_branch: false,
        integrated: false,
        default_branch: default_branch.map(str::to_owned),
    };
    if !off_runtime::exists(&check.workspace.path).await? {
        return Ok(check);
    }
    // Nothing in a disposable workspace is worth keeping, so nothing is
    // inspected for unsaved work; whether it is in use still is.
    if check.workspace.disposable {
        check.branch = check.workspace.branch.clone();
        if policy == InspectionPolicy::IncludeDirectoryProcesses {
            check
                .load_directory_processes(&process::DirectoryUsers::default())
                .await?;
        }
        return Ok(check);
    }
    let path = &check.workspace.path;
    check.dirty = !git::status::porcelain(path).await?.is_empty();
    let branch = git::current_branch(path).await?;
    let branch = branch.trim_end_matches('\n');
    check.branch = (!branch.is_empty()).then(|| branch.to_owned());
    check.head = Some(git::resolve_commit(path, "HEAD", git::run).await?);
    let tree = git::status::head_tree(path).await?;
    // The default branch may be absent locally (workspaces created from another ref).
    let default_refs = match default_branch {
        Some(name) => git::status::default_refs(path, name).await?,
        None => Vec::new(),
    };
    check.matches_default_branch = git::status::retains(path, &default_refs, "HEAD", &tree).await?;
    // Merged changes are on the default branch even when their commits are not.
    if !check.matches_default_branch {
        check.unpushed_commits = git::status::unretained_commits(
            path,
            "HEAD",
            &kept_refs(&check.workspace, &default_refs),
        )
        .await?;
        check.integrated = check.unpushed_commits > 0
            && start_branch_retains_head(&check.workspace, default_branch, &tree).await?;
    }
    if policy == InspectionPolicy::IncludeDirectoryProcesses {
        check
            .load_directory_processes(&process::DirectoryUsers::default())
            .await?;
    }
    Ok(check)
}

/// What keeps a workspace's commits besides the remotes: the local default
/// branch, and for a workspace at a commit the commit it started at, such as
/// a fork PR's head or a local commit no remote has.
fn kept_refs<'a>(workspace: &'a Workspace, default_refs: &'a [String]) -> Vec<&'a str> {
    default_refs
        .first()
        .map(String::as_str)
        .into_iter()
        .chain(workspace.at.as_ref().map(|at| at.commit.as_str()))
        .collect()
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
