//! What removing a workspace would discard, and the caller's branch decision.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{git, model::Workspace, process};

#[derive(Debug, Serialize, Deserialize)]
pub struct RemovalCheck {
    pub workspace: Workspace,
    pub running_commands: usize,
    /// Processes using the directory; only gathered for automatic cleanup.
    pub processes: Vec<String>,
    pub dirty: bool,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct RemovalResult {
    pub removed: bool,
    pub branch: Option<String>,
    pub branch_deleted: bool,
    pub branch_outcome: String,
    /// Removal succeeded, but its best-effort post hook failed.
    pub hook_error: Option<String>,
}

impl RemovalCheck {
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

/// Gather Git state for a removal decision. `caller_pid == 0` marks automatic
/// cleanup, which also treats processes in the directory as activity.
pub async fn inspect(
    workspace: Workspace,
    running_commands: usize,
    caller_pid: u32,
    default_branch: Option<&str>,
) -> Result<RemovalCheck> {
    let mut check = RemovalCheck {
        workspace,
        running_commands,
        processes: vec![],
        dirty: false,
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
    // Processes block automatic cleanup, never manual removal.
    if caller_pid == 0 {
        check.processes = process::in_directory(path).await?;
    }
    Ok(check)
}
