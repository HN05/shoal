//! What a stop saved for `shoal resume`: keeping stopped agents as last
//! sessions after `shoal stop`, and forgetting them, which the daemon does so
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
    /// Keep the workspace's stopped agents whose execution ended as last
    /// sessions, which hold no agent slot and nothing restores on its own. One
    /// a wrapper waits to restore stays, and one that cannot be kept, such as
    /// one a resume holds, stays stopped work.
    pub(super) async fn keep_stopped_as_last_sessions(&self, selector: &str) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        let executions = self.workspace_executions(&workspace.id).await?;
        let mut saved = Saved::load(&self.paths, &workspace.id)?;
        saved.retain_unattended();
        for (id, path) in saved.agents {
            if executions.iter().any(|execution| execution.id == id) {
                continue;
            }
            if let Err(error) =
                recovery::keep_as_last_session(&self.paths, &workspace.id, &id, &path)
            {
                crate::daemon::log!("stopped agent {id} not kept as a last session: {error:#}");
            }
        }
        Ok(())
    }

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
