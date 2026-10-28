//! Forgetting what a stop saved for `shoal resume`, which the daemon does so
//! it can confine coordinating agents to the workspaces below theirs.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::Manager;
use crate::execution::recovery::{self, Saved};

/// What `shoal resume --discard` forgot in a workspace.
#[derive(Debug, Serialize, Deserialize)]
pub struct Discarded {
    /// The workspace's name.
    pub workspace: String,
    /// The execution IDs of the forgotten stopped agents, last sessions and
    /// commands.
    pub executions: Vec<String>,
}

impl Manager {
    /// Forget the workspace's stopped agents, last sessions and commands, only
    /// the one of `execution` when given.
    pub async fn discard_stopped_work(
        &self,
        selector: &str,
        execution: Option<String>,
    ) -> Result<Discarded> {
        let workspace = self.workspace(selector).await?;
        let mut saved = Saved::load(&self.paths, &workspace.id)?;
        if let Some(execution) = &execution {
            saved.retain_execution(execution, false);
        }
        ensure!(
            !saved.is_empty(),
            "no stopped agents or commands for this workspace"
        );
        let executions = self.workspace_executions(&workspace.id).await?;
        let active = |id: &str| executions.iter().any(|e| e.id == id);
        Ok(Discarded {
            workspace: workspace.name,
            executions: recovery::discard_saved(&saved, active)?,
        })
    }
}
