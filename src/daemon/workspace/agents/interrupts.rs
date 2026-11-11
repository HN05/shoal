//! Interrupted turns: the CLI types an agent's interrupt key into the place
//! it runs in, which ends its turn without the hook that ends its other
//! turns, so the daemon records that turn's end and whether the agent still
//! idles after it.
use crate::client::execution_ids::ExecutionId;
use anyhow::{Result, ensure};

use super::super::Manager;
use crate::client::space_names::SpaceSelector;
use crate::model::{AgentStatus, PresentSpace, RunningAgent};

impl Manager {
    /// The agents connected in the space `selector` names, a workspace's or
    /// a running chat's, with the turns they reported and the places they
    /// run in.
    pub(crate) async fn space_agents(&self, selector: &SpaceSelector) -> Result<Vec<RunningAgent>> {
        let space = self.space(selector).await?;
        let agents = match &space {
            PresentSpace::Workspace(_) => self.running_agents().await?,
            PresentSpace::Chat(_) => self.running_chat_agents().await?,
        };
        Ok(agents
            .into_iter()
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
        let reporting = self.reporting_execution(&space, execution).await?;
        self.end_interrupted_turn(&SpaceSelector::id(space.id()), reporting)
            .await
    }

    /// The execution the connected agent `execution` names in `space`
    /// reports its turns from: itself in a workspace, and none for a chat's
    /// agent, which runs in none.
    async fn reporting_execution(
        &self,
        space: &PresentSpace,
        execution: ExecutionId,
    ) -> Result<Option<ExecutionId>> {
        let runs_here = match space {
            PresentSpace::Workspace(workspace) => {
                (self.executions)
                    .with_agent(&execution, |agent| agent.workspace_id == workspace.id)
                    .await
                    == Some(true)
            }
            PresentSpace::Chat(chat) => {
                chat.restore.is_none()
                    && execution == crate::daemon::chats::listed_execution(&chat.id)
            }
        };
        ensure!(
            runs_here,
            "no connected agent of {} runs execution {execution}",
            space.name()
        );
        Ok(matches!(space, PresentSpace::Workspace(_)).then_some(execution))
    }

    /// Whether the agent of `execution` idles after an interrupt ended its
    /// last turn.
    pub(crate) async fn turn_interrupted(&self, execution: &ExecutionId) -> bool {
        self.executions
            .with_agent(execution, |agent| agent.interrupted)
            .await
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests;
