//! The session a new agent starts from a fork of: the one a space's agent
//! reported last, which its hooks keep for `shoal restore`.
use anyhow::{Context as _, Result, bail};

use super::super::manager::Manager;
use crate::{
    execution::recovery,
    model::{AgentSession, Space},
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// The session of the agent in `selector` that a fork starts from: that
    /// of the agent execution `caller` runs there, else of the space's only
    /// agent.
    pub(crate) async fn agent_session(
        &self,
        selector: &SpaceSelector,
        caller: Option<&ExecutionId>,
    ) -> Result<AgentSession> {
        let space = self.space(selector).await?;
        let (execution, agent) = self.forked_agent(&space, caller).await?;
        let session =
            recovery::reported_session(&self.paths, &space.id, &execution).with_context(|| {
                format!(
                    "{agent} in {} has not reported a session to fork",
                    space.name
                )
            })?;
        Ok(AgentSession { agent, session })
    }

    /// The execution and name of the agent in `space` that a fork starts
    /// from, refusing to choose between several.
    async fn forked_agent(
        &self,
        space: &Space,
        caller: Option<&ExecutionId>,
    ) -> Result<(ExecutionId, crate::agent::Agent)> {
        let mut running: Vec<(ExecutionId, crate::agent::Agent)> = self
            .executions
            .with_agents(|agents| {
                agents
                    .iter()
                    .filter(|(_, agent)| agent.workspace_id == space.id)
                    // Named as `--agent` and recovery records name it.
                    .map(|(id, agent)| (id.clone(), agent.name.normalized()))
                    .collect()
            })
            .await;
        if let Some(own) = running.iter().position(|(id, _)| Some(id) == caller) {
            return Ok(running.swap_remove(own));
        }
        match running.len() {
            1 => Ok(running.swap_remove(0)),
            0 => bail!("{} has no running agent to fork", space.name),
            _ => bail!(
                "{} runs several agents; fork one from inside it",
                space.name
            ),
        }
    }
}

#[cfg(test)]
mod tests;
