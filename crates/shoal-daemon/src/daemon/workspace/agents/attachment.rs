//! Whether a terminal shows each tracked agent, as its wrapper reports, so
//! protection stops the agents the user is not working with first.
use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

use super::Manager;

impl Manager {
    /// Record whether a terminal shows the agent `execution` runs in the
    /// workspace `selector` names. A report for an agent the daemon no
    /// longer tracks there changes nothing.
    pub(crate) async fn report_attached(
        &self,
        selector: &SpaceSelector,
        execution: &ExecutionId,
        attached: bool,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.executions
            .with_agent(execution, |agent| {
                if agent.workspace_id == workspace.id {
                    agent.attached = Some(attached);
                }
            })
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
