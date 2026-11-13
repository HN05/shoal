//! Interrupted turns: the CLI types an agent's interrupt key into the place
//! it runs in, which ends its turn without the hook that ends its other
//! turns, so the daemon records that turn's end and whether the agent still
//! idles after it.
use crate::client::execution_ids::ExecutionId;
use anyhow::{Result, ensure};

use super::super::Manager;
use crate::client::space_names::SpaceSelector;
use crate::model::{AgentStatus, RunningAgent};

impl Manager {
    /// The agents connected in the space `selector` names, a workspace's or
    /// a running chat's, with the turns they reported and the places they
    /// run in.
    pub(crate) async fn space_agents(&self, selector: &SpaceSelector) -> Result<Vec<RunningAgent>> {
        let space = self.space(selector).await?;
        Ok((self.running_agents().await?.into_iter())
            .filter(|agent| agent.space_id == *space.id() && agent.agent.is_some())
            .collect())
    }

    /// Record that an interrupt ended the turn of the agent `execution`
    /// runs, as [`Self::space_agents`] lists it, in the space `selector`
    /// names.
    pub(crate) async fn interrupt_turn(
        &self,
        selector: &SpaceSelector,
        execution: ExecutionId,
    ) -> Result<AgentStatus> {
        let space = self.space(selector).await?;
        let runs_here = (self.executions)
            .with_agent(&execution, |agent| agent.workspace_id == *space.id())
            .await
            == Some(true);
        ensure!(
            runs_here,
            "no connected agent of {} runs execution {execution}",
            space.name()
        );
        (self.end_interrupted_turn(&SpaceSelector::id(space.id()), Some(execution))).await
    }

    /// Whether the agent of `execution` idles after an interrupt ended its
    /// last turn.
    pub(crate) async fn turn_interrupted(&self, execution: &ExecutionId) -> bool {
        let id = execution.to_owned();
        let interrupted = (self.store)
            .read(move |db| super::super::agent_state::interrupted(db, &id))
            .await;
        interrupted.unwrap_or_else(|error| {
            crate::daemon::log!("interrupt of execution {execution} unreadable: {error:#}");
            false
        })
    }
}

#[cfg(test)]
mod tests;
