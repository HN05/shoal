//! Whether a terminal shows each tracked agent, as its wrapper reports.
use anyhow::Result;

use super::Manager;

impl Manager {
    /// Record whether a terminal shows the agent `execution` runs in the
    /// workspace `selector` names. A report for an agent the daemon no
    /// longer tracks there changes nothing.
    pub(crate) async fn report_attached(
        &self,
        selector: &str,
        execution: &str,
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
