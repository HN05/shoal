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

/// A workspace process that stopped another workspace's executions, which
/// may resume the agents among them.
pub(super) struct Stopper<'a> {
    /// The ID of the process's workspace.
    pub(super) workspace: &'a str,
    /// The executions the stop ended.
    pub(super) executions: Vec<String>,
}

impl Manager {
    /// The executions of workspace `id` that nothing stopped yet. An agent
    /// that already saved its recovery record was stopped before, such as
    /// one whose wrapper waits to restore it after a protection stop.
    pub(super) async fn unstopped_executions(&self, id: &str) -> Result<Vec<String>> {
        let executions = self.workspace_executions(id).await?;
        let stopped_before =
            |execution: &str| recovery::record_path(&self.paths, id, execution).exists();
        Ok(executions
            .into_iter()
            .map(|execution| execution.id)
            .filter(|execution| !stopped_before(execution))
            .collect())
    }

    /// Keep the workspace's stopped agents whose execution ended as last
    /// sessions, which hold no agent slot and nothing restores on its own,
    /// recording `stopper` on those it stopped. One a wrapper waits to
    /// restore stays, and one that cannot be kept, such as one a resume
    /// holds, stays stopped work.
    pub(super) async fn keep_stopped_as_last_sessions(
        &self,
        selector: &str,
        stopper: Option<&Stopper<'_>>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        let executions = self.workspace_executions(&workspace.id).await?;
        let mut saved = Saved::load(&self.paths, &workspace.id)?;
        saved.retain_unattended();
        for (id, path) in saved.agents {
            if executions.iter().any(|execution| execution.id == id) {
                continue;
            }
            let stopped_by = stopper
                .filter(|stopper| stopper.executions.contains(&id))
                .map(|stopper| stopper.workspace);
            if let Err(error) =
                recovery::keep_as_last_session(&self.paths, &workspace.id, &id, &path, stopped_by)
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
        let executions = recovery::discard_saved(&saved, active)?;
        // The saved sessions are files, so no stored change announces it.
        self.signals.agent_changed();
        Ok(Discarded {
            workspace: workspace.name,
            executions,
        })
    }
}
