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
}

impl RemovalCheck {
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.running_commands > 0 {
            warnings.push(format!(
                "{} Shoal command(s) are running and will be stopped",
                self.running_commands
            ));
        }
        if !self.processes.is_empty() {
            warnings.push(format!("Processes are using this directory: {}. Only Shoal-managed commands will be stopped", self.processes.join(", ")));
        }
        if self.dirty {
            warnings.push("Uncommitted changes or untracked files will be deleted".into());
        }
        if self.unpushed_commits > 0 {
            warnings.push(format!(
                "{} commit(s) are not on any known remote branch; the Git branch will be retained",
                self.unpushed_commits
            ));
        }
        warnings
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
        check.processes = processes::in_directory(&check.workspace.path, caller_pid).await?;
    }
    Ok(check)
}
