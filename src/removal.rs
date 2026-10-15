//! What removing a workspace would discard, and the caller's branch decision.
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
    /// Commits reachable from neither a remote-tracking branch nor the local
    /// default branch, so removing the worktree could lose them.
    pub unpushed_commits: u64,
    pub branch: Option<String>,
    /// HEAD's tree equals the local default branch's, or is already merged into it.
    pub matches_default_branch: bool,
    pub matches_upstream: bool,
    /// The default branch used by this inspection, when it could be resolved.
    ///
    /// This is daemon state used to keep the removal decision consistent with
    /// the inspection; it is not part of the CLI protocol response.
    #[serde(skip)]
    pub(crate) default_branch: Option<String>,
}

/// What happens to the workspace branch when its worktree is removed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchChoice {
    /// Delete the branch only when it is redundant with the default branch or
    /// its upstream; otherwise a choice is required.
    Auto,
    KeepBranch,
    DeleteBranch,
}

crate::state::open_enum! {
    /// Worktrunk 0.78.0's `BranchFate::json_outcome` vocabulary, plus Shoal's
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
        self.processes = process::in_directory(&self.workspace.path).await?;
        Ok(())
    }

    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.dirty {
            warnings.push("Uncommitted changes or untracked files will be deleted".into());
        }
        if !self.matches_default_branch && !self.matches_upstream {
            warnings.push(
                "Branch contents differ from the default branch and upstream (or those refs are unavailable)"
                    .into(),
            );
        }
        warnings
    }

    pub fn can_delete_branch(&self) -> bool {
        !self.dirty && (self.matches_default_branch || self.matches_upstream)
    }

    pub fn needs_choice(&self) -> bool {
        self.workspace.path.exists() && !self.can_delete_branch()
    }

    /// Nothing would be lost: idle, clean, and every commit pushed or landed.
    pub fn safe(&self) -> bool {
        self.running_commands == 0
            && self.processes.is_empty()
            && !self.dirty
            && self.unpushed_commits == 0
    }
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

/// Gather state for a removal decision without deciding whether removal is safe.
pub async fn inspect(
    workspace: Workspace,
    running_commands: usize,
    policy: InspectionPolicy,
    default_branch: Option<&str>,
) -> Result<RemovalCheck> {
    let mut check = RemovalCheck {
        workspace,
        running_commands,
        processes: vec![],
        dirty: false,
        changed_files: vec![],
        changed_files_omitted: 0,
        unpushed_commits: 0,
        branch: None,
        matches_default_branch: false,
        matches_upstream: false,
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
    let tree = git::revision(path, "HEAD^{tree}", git::run).await?;
    // The default branch may be absent locally (workspaces created from another ref).
    let default_ref = match default_branch {
        Some(name) => {
            let reference = git::local_ref(name);
            git::ref_exists(path, &reference, git::command)
                .await?
                .then_some(reference)
        }
        None => None,
    };
    if let Some(default_ref) = &default_ref {
        check.matches_default_branch =
            git::verify_revision(path, &format!("{default_ref}^{{tree}}"), git::run)
                .await
                .is_ok_and(|other| other == tree)
                || git::is_ancestor(path, "HEAD", default_ref, git::command).await?;
    }
    check.unpushed_commits = git::status::unretained_commits(path, default_ref.as_deref()).await?;
    check.matches_upstream = git::verify_revision(path, "@{upstream}^{tree}", git::run)
        .await
        .is_ok_and(|other| other == tree);
    if policy == InspectionPolicy::IncludeDirectoryProcesses {
        check.load_directory_processes().await?;
    }
    Ok(check)
}
