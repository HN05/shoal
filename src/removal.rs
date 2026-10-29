//! What removing a workspace would discard, and whether the caller lets it.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{git, model::Workspace, process};

#[cfg(test)]
mod tests;

#[derive(Debug, Serialize, Deserialize)]
pub struct RemovalCheck {
    pub workspace: Workspace,
    pub running_commands: usize,
    /// Processes using the directory, when requested by the inspection policy.
    pub processes: Vec<String>,
    pub dirty: bool,
    /// Git porcelain status lines, with quoted paths and individual untracked files.
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub changed_files_omitted: usize,
    /// A checkpoint would keep the unsaved files, when requested with them.
    #[serde(default)]
    pub unsaved_files_archived: bool,
    /// Commits reachable from neither a remote-tracking branch nor the local
    /// default branch, so removing the worktree could lose them; zero when the
    /// default branch already has HEAD's changes.
    pub unpushed_commits: u64,
    pub branch: Option<String>,
    /// HEAD's commit.
    #[serde(default)]
    pub head: Option<String>,
    /// The local default branch or its remote-tracking branch records HEAD's
    /// tree or has its changes, merged, squashed or rebased.
    pub matches_default_branch: bool,
    /// The local branch the workspace builds on, other than the default
    /// branch, has HEAD's changes that neither a remote nor the default
    /// branch has: they are kept there under these or other commits.
    #[serde(default)]
    pub integrated: bool,
    /// The default branch used by this inspection, when it could be resolved.
    ///
    /// This is daemon state used to keep the removal decision consistent with
    /// the inspection; it is not part of the CLI protocol response.
    #[serde(skip)]
    pub(crate) default_branch: Option<String>,
    /// The restores of idle agents, which `running_commands` and
    /// `processes` leave out; daemon state like `default_branch`.
    #[serde(skip)]
    pub(crate) idle_restores: Vec<IdleRestore>,
}

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

/// What a manual removal may delete that neither a branch nor a remote
/// keeps. The removal's checkpoint keeps it until it expires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsavedWork {
    /// What the caller saw and confirmed: uncommitted and untracked files,
    /// and the commits of a detached HEAD while HEAD is still that commit.
    /// `files_restorable` records that the caller was told the checkpoint
    /// keeps the files, so removal refuses when it cannot after all.
    Confirmed {
        files: bool,
        detached_head: Option<String>,
        #[serde(default)]
        files_restorable: bool,
    },
    /// Everything, for removals that delete more than the workspace.
    All,
}

/// Consent to delete nothing.
pub static NOTHING: UnsavedWork = UnsavedWork::Confirmed {
    files: false,
    detached_head: None,
    files_restorable: false,
};

impl Default for UnsavedWork {
    fn default() -> Self {
        NOTHING.clone()
    }
}

impl UnsavedWork {
    /// Consent to delete the unsaved work `check` shows.
    pub fn confirming(check: &RemovalCheck) -> Self {
        Self::Confirmed {
            files: check.dirty,
            detached_head: check.head.clone().filter(|_| check.detached_commits() > 0),
            files_restorable: check.dirty && check.unsaved_files_archived,
        }
    }

    /// The caller was told the removal's checkpoint keeps the files.
    pub fn promises_restorable_files(&self) -> bool {
        matches!(
            self,
            Self::Confirmed {
                files_restorable: true,
                ..
            }
        )
    }

    /// The unsaved work in `check` this consent does not cover, as
    /// [`RemovalCheck::unsaved_work`] describes it.
    pub fn uncovered(&self, check: &RemovalCheck) -> Vec<String> {
        match self {
            Self::All => Vec::new(),
            Self::Confirmed {
                files,
                detached_head,
                ..
            } => {
                let commits = if detached_head.is_some() && *detached_head == check.head {
                    0
                } else {
                    check.detached_commits()
                };
                describe_unsaved(check.dirty && !files, commits)
            }
        }
    }
}

crate::state::open_enum! {
    /// Worktrunk 0.81.0's `BranchFate::json_outcome` vocabulary, plus Shoal's
    /// synthetic outcome for an already-missing worktree registration. Future
    /// Worktrunk values decode as `Unknown` without claiming confirmed deletion.
    BranchOutcome {
        Deleted => "deleted",
        NotAttempted => "not_attempted",
        Deferred => "deferred",
        RetainedUnmerged => "retained_unmerged",
        RetainedCheckedOut => "retained_checked_out",
        RetainedRaced => "retained_raced",
        RetainedFailed => "retained_failed",
        Retained => "retained",
    }
}

impl BranchOutcome {
    pub fn is_deleted(&self) -> bool {
        matches!(self, Self::Deleted)
    }
}

#[derive(Debug, Deserialize)]
pub struct RemovalResult {
    pub removed: bool,
    pub branch: Option<String>,
    pub branch_outcome: BranchOutcome,
    /// Removal succeeded, but its best-effort post hook failed.
    pub hook_error: Option<String>,
}

impl Serialize for RemovalResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Keep the existing response shape, but never store a second source of
        // truth for deletion. Deserialization likewise uses only the outcome.
        #[derive(Serialize)]
        struct Response<'a> {
            removed: bool,
            branch: &'a Option<String>,
            branch_deleted: bool,
            branch_outcome: &'a BranchOutcome,
            hook_error: &'a Option<String>,
        }
        Response {
            removed: self.removed,
            branch: &self.branch,
            branch_deleted: self.branch_outcome.is_deleted(),
            branch_outcome: &self.branch_outcome,
            hook_error: &self.hook_error,
        }
        .serialize(serializer)
    }
}

impl RemovalCheck {
    pub async fn load_changed_files(&mut self) -> Result<()> {
        let status = git::status::changed_files(&self.workspace.path).await?;
        (self.changed_files, self.changed_files_omitted) = changed_files_preview(&status);
        Ok(())
    }

    pub async fn load_directory_processes(&mut self) -> Result<()> {
        let wrappers: Vec<u32> = self
            .idle_restores
            .iter()
            .filter_map(|restore| restore.wrapper)
            .collect();
        self.processes = process::in_directory_except(&self.workspace.path, &wrappers).await?;
        Ok(())
    }

    /// What removing the worktree would delete that no branch or remote
    /// keeps, one entry per kind; empty when removal loses nothing.
    pub fn unsaved_work(&self) -> Vec<String> {
        describe_unsaved(self.dirty, self.detached_commits())
    }

    /// Commits on a detached HEAD that no branch or remote has.
    fn detached_commits(&self) -> u64 {
        if self.branch.is_none() {
            self.unpushed_commits
        } else {
            0
        }
    }

    /// The branch can go with the worktree: a remote or the default branch
    /// has every one of its commits.
    pub fn branch_preserved(&self) -> bool {
        self.branch.is_some() && self.unpushed_commits == 0
    }

    /// No command or process uses the worktree.
    pub fn unused(&self) -> bool {
        self.running_commands == 0 && self.processes.is_empty()
    }

    /// Every commit is pushed or landed, or the branch the workspace builds
    /// on has their changes.
    pub fn commits_kept(&self) -> bool {
        self.unpushed_commits == 0 || self.integrated
    }

    /// Nothing would be lost: idle, clean, and every commit kept.
    pub fn safe(&self) -> bool {
        self.unused() && !self.dirty && self.commits_kept()
    }
}

fn describe_unsaved(files: bool, detached_commits: u64) -> Vec<String> {
    let mut work = Vec::new();
    if files {
        work.push("uncommitted changes or untracked files".into());
    }
    if detached_commits > 0 {
        work.push(format!(
            "{detached_commits} commit(s) on a detached HEAD that no branch or remote has"
        ));
    }
    work
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
    if !check.workspace.path.exists() {
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
        check.unpushed_commits =
            git::status::unretained_commits(path, "HEAD", default_refs.first().map(String::as_str))
                .await?;
        check.integrated = check.unpushed_commits > 0
            && start_branch_retains_head(&check.workspace, default_branch, &tree).await?;
    }
    if policy == InspectionPolicy::IncludeDirectoryProcesses {
        check.load_directory_processes().await?;
    }
    Ok(check)
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
        Some(base) => base.branch.as_str(),
        None => workspace
            .base_ref
            .as_deref()?
            .strip_prefix(git::LOCAL_REFS)?,
    };
    (branch != workspace.branch).then_some(branch)
}
