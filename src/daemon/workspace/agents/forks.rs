//! The session a new agent starts from a fork of: the one a workspace's
//! agent reported last, which its hooks keep for `shoal resume`.
use anyhow::{Context as _, Result, bail};

use super::super::Manager;
use crate::{
    execution::recovery,
    model::{AgentSession, Workspace},
};

impl Manager {
    /// The session of the agent in `selector` that a fork starts from: that
    /// of the agent execution `caller` runs there, else of the workspace's
    /// only agent.
    pub(crate) async fn agent_session(
        &self,
        selector: &str,
        caller: Option<&str>,
    ) -> Result<AgentSession> {
        let workspace = self.workspace(selector).await?;
        let (execution, agent) = self.forked_agent(&workspace, caller).await?;
        let session = recovery::reported_session(&self.paths, &workspace.id, &execution)
            .with_context(|| {
                format!(
                    "{agent} in {} has not reported a session to fork",
                    workspace.name
                )
            })?;
        Ok(AgentSession { agent, session })
    }

    /// The execution and name of the agent in `workspace` that a fork starts
    /// from, refusing to choose between several.
    async fn forked_agent(
        &self,
        workspace: &Workspace,
        caller: Option<&str>,
    ) -> Result<(String, String)> {
        let mut running: Vec<(String, String)> = self
            .executions
            .with_agents(|agents| {
                agents
                    .iter()
                    .filter(|(_, agent)| agent.workspace_id == workspace.id)
                    // Named as `--agent` and recovery records name it.
                    .map(|(id, agent)| (id.clone(), agent.name.replace(' ', "-")))
                    .collect()
            })
            .await;
        if let Some(own) = running
            .iter()
            .position(|(id, _)| Some(id.as_str()) == caller)
        {
            return Ok(running.swap_remove(own));
        }
        match running.len() {
            1 => Ok(running.swap_remove(0)),
            0 => bail!("{} has no running agent to fork", workspace.name),
            _ => bail!(
                "{} runs several agents; fork one from inside it",
                workspace.name
            ),
        }
    }
}

#[cfg(test)]
mod tests;
