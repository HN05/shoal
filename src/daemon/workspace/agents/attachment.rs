//! Whether a terminal shows each tracked agent, as its wrapper reports, and
//! what its hooks told it of that, so it asks the user in its chat while
//! the user is there.
use anyhow::Result;

use super::Manager;
use crate::daemon::links::wait::Updates;

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

    /// The updates of the workspace `selector` names for a hook of the agent
    /// `execution` runs, with whether a terminal shows the agent when that
    /// changed since its hooks last told it.
    pub(crate) async fn agent_updates_for(
        &self,
        selector: &str,
        execution: Option<&str>,
    ) -> Result<Updates> {
        let mut updates = self.agent_updates(selector).await?;
        if let Some(execution) = execution.filter(|_| !updates.superseded) {
            updates.attached = self.attachment_news(execution).await;
        }
        Ok(updates)
    }

    /// Whether a terminal shows the agent `execution` runs, when that
    /// changed since its hooks last told it.
    async fn attachment_news(&self, execution: &str) -> Option<bool> {
        self.executions
            .with_agent(execution, |agent| {
                agent
                    .attached
                    .filter(|attached| agent.told_attached != Some(*attached))
            })
            .await
            .flatten()
    }

    /// Record that the hooks of the agent `execution` runs told it whether a
    /// terminal shows it, when a delivery did.
    pub(crate) async fn told_attached(&self, execution: Option<&str>, attached: Option<bool>) {
        let (Some(execution), Some(attached)) = (execution, attached) else {
            return;
        };
        self.executions
            .with_agent(execution, |agent| agent.told_attached = Some(attached))
            .await;
    }
}

#[cfg(test)]
mod tests;
