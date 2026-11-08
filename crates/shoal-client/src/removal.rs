//! What removing a workspace would discard, whether the caller lets it, and
//! what the removal did.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::space_names::SpaceSelector;
use crate::{
    cleanup::{ManualCleanup, Selection},
    connection::{Daemon, pages, request},
    model::{RemovedWorkspace, RestoredWorkspace, Workspace},
    protocol::RemovalMethod,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Serialize, Deserialize)]
pub struct RemovalCheck {
    pub workspace: Workspace,
    pub running_commands: usize,
    /// Processes using the directory, when requested by the inspection policy.
    pub processes: Vec<String>,
    /// Uncommitted or untracked files of its own.
    pub dirty: bool,
    /// Uncommitted or untracked files that are those a copy started with,
    /// which its starting checkpoint keeps, so removal deletes them as it
    /// deletes nothing of the workspace's own.
    #[serde(default)]
    pub starting_files: bool,
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
    /// Every commit no remote or default branch keeps is one a copy
    /// started with, which its starting checkpoint keeps: cleanup may remove
    /// the worktree, while removal keeps the branch.
    #[serde(default)]
    pub starting_commits: bool,
    /// The spaces below it, which removal takes first, each after those
    /// below it; filled in for a manual removal's check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<crate::model::SpaceRef>,
    /// The IDs of the `children` that are disposable, which removal takes
    /// without asking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disposable_children: Vec<String>,
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
    /// `children` are the IDs of the spaces below it the caller saw, the
    /// only ones it may take; without them it takes whatever is below.
    Confirmed {
        files: bool,
        detached_head: Option<String>,
        #[serde(default)]
        files_restorable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        children: Option<Vec<String>>,
    },
    /// Everything, for removals that delete more than the workspace.
    All,
}

/// Consent to delete nothing.
pub static NOTHING: UnsavedWork = UnsavedWork::Confirmed {
    files: false,
    detached_head: None,
    files_restorable: false,
    children: None,
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
            children: Some(
                check
                    .children
                    .iter()
                    .map(|child| child.id.clone())
                    .collect(),
            ),
        }
    }

    /// The IDs of the workspaces below it the caller saw and lets removal
    /// take, or `None` when it may take any.
    pub fn children(&self) -> Option<&[String]> {
        match self {
            Self::Confirmed { children, .. } => children.as_deref(),
            Self::All => None,
        }
    }

    /// Whether it consents to deleting any unsaved work, which removal then
    /// forces; the children it names are no unsaved work.
    pub fn deletes_unsaved(&self) -> bool {
        match self {
            Self::All => true,
            Self::Confirmed {
                files,
                detached_head,
                ..
            } => *files || detached_head.is_some(),
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

#[derive(Debug, Clone, Deserialize)]
pub struct RemovalResult {
    pub removed: bool,
    pub branch: Option<String>,
    pub branch_outcome: BranchOutcome,
    /// Removal succeeded, but its best-effort post hook failed.
    pub hook_error: Option<String>,
    /// The workspaces below it the removal took first, each after those
    /// below it.
    #[serde(default)]
    pub children: Vec<String>,
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
            children: &'a [String],
        }
        Response {
            removed: self.removed,
            branch: &self.branch,
            branch_deleted: self.branch_outcome.is_deleted(),
            branch_outcome: &self.branch_outcome,
            hook_error: &self.hook_error,
            children: &self.children,
        }
        .serialize(serializer)
    }
}

impl RemovalCheck {
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

    /// Every commit is pushed or merged, or the branch the workspace builds
    /// on has their changes, or a copy started with them.
    pub fn commits_kept(&self) -> bool {
        self.unpushed_commits == 0 || self.integrated || self.starting_commits
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

/// What removing `workspace` would discard, with its changes when
/// `include_changes`.
pub async fn check(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    include_changes: bool,
) -> Result<RemovalCheck> {
    let method = RemovalMethod::CheckRemoval {
        workspace,
        include_changes,
    };
    request(daemon, method).await
}

/// Remove `workspace`, deleting only the `unsaved` work the caller saw.
pub async fn remove(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    unsaved: UnsavedWork,
) -> Result<RemovalResult> {
    request(
        daemon,
        RemovalMethod::RemoveWorkspace { workspace, unsaved },
    )
    .await
}

/// Remove what `selection` selects that nothing keeps, or with `dry_run`
/// only list it.
pub async fn clean_up(
    daemon: &impl Daemon,
    dry_run: bool,
    selection: Selection,
) -> Result<ManualCleanup> {
    request(daemon, RemovalMethod::Cleanup { dry_run, selection }).await
}

/// The removed workspaces whose records allow restoring them.
pub async fn removed(daemon: &impl Daemon) -> Result<Vec<RemovedWorkspace>> {
    pages(daemon, RemovalMethod::RemovedWorkspaces).await
}

/// Open the removed `workspace` again from its record.
pub async fn restore(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
) -> Result<Box<RestoredWorkspace>> {
    request(daemon, RemovalMethod::RestoreWorkspace { workspace }).await
}

/// Delete the session files the restore that opened `workspace` returned,
/// which the caller has put in place.
pub async fn drop_restored_sessions(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<()> {
    request(daemon, RemovalMethod::DropRestoredSessions { workspace }).await
}
