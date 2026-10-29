//! Interrupted turns: the CLI types an agent's interrupt key into the place
//! it runs in, which ends its turn without the hook that ends its other
//! turns, so the daemon records that turn's end and whether the agent still
//! idles after it.
use anyhow::{Result, ensure};

use super::super::Manager;
use crate::model::{AgentStatus, RunningAgent};

impl Manager {
    /// The agents connected in the workspace `selector` names, with the
    /// turns they reported and the places they run in.
    pub(crate) async fn workspace_agents(&self, selector: &str) -> Result<Vec<RunningAgent>> {
        let workspace = self.workspace(selector).await?;
        Ok(self
            .running_agents()
            .await?
            .into_iter()
            .filter(|agent| agent.workspace_id == workspace.id && agent.agent.is_some())
            .collect())
    }

    /// Record that an interrupt ended the turn of the agent `execution`
    /// runs in the workspace `selector` names.
    pub(crate) async fn interrupt_turn(
        &self,
        selector: &str,
        execution: String,
    ) -> Result<AgentStatus> {
        let workspace = self.workspace(selector).await?;
        let runs_here = self
            .executions
            .with_agent(&execution, |agent| agent.workspace_id == workspace.id)
            .await;
        ensure!(
            runs_here == Some(true),
            "no connected agent of {} runs execution {execution}",
            workspace.name
        );
        self.end_interrupted_turn(&workspace.id, execution).await
    }

    /// Whether the agent of `execution` idles after an interrupt ended its
    /// last turn.
    pub(crate) async fn turn_interrupted(&self, execution: &str) -> bool {
        self.executions
            .with_agent(execution, |agent| agent.interrupted)
            .await
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests;
