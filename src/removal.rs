use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{model::Workspace, processes, worktrunk};

#[derive(Debug, Serialize, Deserialize)]
pub struct RemovalCheck {
    pub workspace: Workspace,
    pub running_commands: usize,
    pub processes: Vec<String>,
    pub dirty: bool,
    pub unpushed_commits: u64,
    pub branch: Option<String>,
    pub matches_main: bool,
    pub matches_upstream: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
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
}

impl RemovalCheck {
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.dirty {
            warnings.push("Uncommitted changes or untracked files will be deleted".into());
        }
        if !self.matches_main && !self.matches_upstream {
            warnings.push(
                "Branch contents differ from main and upstream (or those refs are unavailable)"
                    .into(),
            );
        }
        warnings
    }

    pub fn can_delete_branch(&self) -> bool {
        !self.dirty && (self.matches_main || self.matches_upstream)
    }

    pub fn needs_choice(&self) -> bool {
        self.workspace.path.exists() && !self.can_delete_branch()
    }

    pub fn safe(&self) -> bool {
        self.running_commands == 0
            && self.processes.is_empty()
            && !self.dirty
            && self.unpushed_commits == 0
    }
}

pub async fn check(
    workspace: Workspace,
    running_commands: usize,
    caller_pid: u32,
) -> Result<RemovalCheck> {
    let mut check = RemovalCheck {
        workspace,
        running_commands,
        processes: vec![],
        dirty: false,
        unpushed_commits: 0,
        branch: None,
        matches_main: false,
        matches_upstream: false,
    };
    if check.workspace.path.exists() {
        check.dirty = !worktrunk::git(
            &check.workspace.path,
            &["status", "--porcelain", "--untracked-files=normal"],
        )
        .await?
        .is_empty();
        check.unpushed_commits = worktrunk::git(
            &check.workspace.path,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
        )
        .await?
        .trim()
        .parse()?;
        let branch = worktrunk::git(&check.workspace.path, &["branch", "--show-current"]).await?;
        check.branch = (!branch.trim().is_empty()).then(|| branch.trim().to_owned());
        let tree = worktrunk::git(&check.workspace.path, &["rev-parse", "HEAD^{tree}"]).await?;
        check.matches_main = worktrunk::git(
            &check.workspace.path,
            &["rev-parse", "--verify", "refs/heads/main^{tree}"],
        )
        .await
        .is_ok_and(|other| other == tree);
        check.matches_upstream = worktrunk::git(
            &check.workspace.path,
            &["rev-parse", "--verify", "@{upstream}^{tree}"],
        )
        .await
        .is_ok_and(|other| other == tree);
        // Processes block automatic cleanup, never manual removal.
        if caller_pid == 0 {
            check.processes = processes::in_directory(&check.workspace.path).await?;
        }
    }
    Ok(check)
}
