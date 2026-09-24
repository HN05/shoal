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

/// Worktrunk 0.78.0's `BranchFate::json_outcome` vocabulary, plus Shoal's
/// synthetic outcome for an already-missing worktree registration.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", from = "String")]
pub enum BranchOutcome {
    Deleted,
    NotAttempted,
    Deferred,
    RetainedUnmerged,
    RetainedCheckedOut,
    RetainedRaced,
    RetainedFailed,
    Retained,
    /// Preserve future Worktrunk values without claiming confirmed deletion.
    #[serde(untagged)]
    Unknown(String),
}

impl From<String> for BranchOutcome {
    fn from(value: String) -> Self {
        match value.as_str() {
            "deleted" => Self::Deleted,
            "not_attempted" => Self::NotAttempted,
            "deferred" => Self::Deferred,
            "retained_unmerged" => Self::RetainedUnmerged,
            "retained_checked_out" => Self::RetainedCheckedOut,
            "retained_raced" => Self::RetainedRaced,
            "retained_failed" => Self::RetainedFailed,
            "retained" => Self::Retained,
            _ => Self::Unknown(value),
        }
    }
}

impl BranchOutcome {
    pub fn is_deleted(&self) -> bool {
        matches!(self, Self::Deleted)
    }
}

impl std::fmt::Display for BranchOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Deleted => "deleted",
            Self::NotAttempted => "not_attempted",
            Self::Deferred => "deferred",
            Self::RetainedUnmerged => "retained_unmerged",
            Self::RetainedCheckedOut => "retained_checked_out",
            Self::RetainedRaced => "retained_raced",
            Self::RetainedFailed => "retained_failed",
            Self::Retained => "retained",
            Self::Unknown(value) => value,
        })
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
        let status = git::run(
            &self.workspace.path,
            &[
                "-c",
                "core.quotePath=true",
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
            ],
        )
        .await?;
        (self.changed_files, self.changed_files_omitted) = changed_files_preview(&status);
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
    };
    if !check.workspace.path.exists() {
        return Ok(check);
    }
    let path = &check.workspace.path;
    check.dirty = !git::run(path, &["status", "--porcelain", "--untracked-files=normal"])
        .await?
        .is_empty();
    let branch = git::run(path, &["branch", "--show-current"]).await?;
    let branch = branch.trim_end_matches('\n');
    check.branch = (!branch.is_empty()).then(|| branch.to_owned());
    let tree = git::run(path, &["rev-parse", "HEAD^{tree}"]).await?;
    // The default branch may be absent locally (workspaces created from another ref).
    let default_ref = match default_branch {
        Some(name) => {
            let reference = git::local_ref(name);
            git::run(path, &["rev-parse", "--verify", &reference])
                .await
                .is_ok()
                .then_some(reference)
        }
        None => None,
    };
    let mut retained = vec!["rev-list", "--count", "HEAD", "--not", "--remotes"];
    if let Some(default_ref) = &default_ref {
        retained.push(default_ref);
        check.matches_default_branch = git::run(
            path,
            &["rev-parse", "--verify", &format!("{default_ref}^{{tree}}")],
        )
        .await
        .is_ok_and(|other| other == tree)
            || git::run(path, &["merge-base", "--is-ancestor", "HEAD", default_ref])
                .await
                .is_ok();
    }
    check.unpushed_commits = git::run(path, &retained).await?.trim().parse()?;
    check.matches_upstream = git::run(path, &["rev-parse", "--verify", "@{upstream}^{tree}"])
        .await
        .is_ok_and(|other| other == tree);
    if policy == InspectionPolicy::IncludeDirectoryProcesses {
        check.processes = process::in_directory(path).await?;
    }
    Ok(check)
}
