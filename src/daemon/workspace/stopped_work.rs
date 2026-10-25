//! Forgetting what a stop saved for `shoal resume`, which the daemon does so
//! it can confine coordinating agents to the workspaces below theirs.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::Manager;
use crate::execution::recovery::{self, LastSession, Saved};

/// What `shoal resume --discard` forgot in a workspace.
#[derive(Debug, Serialize, Deserialize)]
pub struct Discarded {
    /// The workspace's name.
    pub workspace: String,
    /// The execution IDs of the forgotten stopped agents and commands.
    pub executions: Vec<String>,
    /// The agent of the forgotten last session, when nothing was stopped.
    pub last_session: Option<String>,
}

impl Manager {
    /// Forget the workspace's stopped agents and commands, only the one of
    /// `execution` when given, or else the agent that last exited there.
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
        let mut discarded = Discarded {
            workspace: workspace.name,
            executions: Vec::new(),
            last_session: None,
        };
        if saved.is_empty() && execution.is_none() {
            discarded.last_session = self.discard_last_session(&workspace.id)?;
            if discarded.last_session.is_some() {
                return Ok(discarded);
            }
        }
        ensure!(
            !saved.is_empty(),
            "no stopped agents or commands for this workspace"
        );
        let executions = self.workspace_executions(&workspace.id).await?;
        let active = |id: &str| executions.iter().any(|e| e.id == id);
        discarded.executions = recovery::discard_saved(&saved, active)?;
        Ok(discarded)
    }

    /// Forget the workspace's last session, returning its agent.
    fn discard_last_session(&self, workspace_id: &str) -> Result<Option<String>> {
        let Some(last) = LastSession::load(&self.paths, workspace_id)? else {
            return Ok(None);
        };
        let path = LastSession::path(&self.paths, workspace_id);
        let _claim = recovery::claim(&path)?;
        recovery::consume(&path)?;
        Ok(Some(last.agent))
    }
}
