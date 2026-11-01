//! Queuing an agent in an existing workspace in one request, as `shoal split
//! --agent` queues a child's.
use anyhow::{Context as _, Result};

use crate::{
    agent::Agent,
    daemon::{operations::starts::check_agent, workspace::Manager},
    model::QueueEntry,
    protocol::operations::QueuedLaunch,
};

impl Manager {
    /// Queue `agent` in the ready workspace `workspace`, with `prompt` as its
    /// first message, once it can run there.
    pub(crate) async fn queue_agent(
        &self,
        workspace: &str,
        agent: String,
        prompt: Option<String>,
    ) -> Result<QueueEntry> {
        let workspace = self.workspace(workspace).await?;
        let launch = QueuedLaunch { agent, prompt };
        check_agent(&launch, &self.workspace_settings(&workspace).await?)?;
        let agent: Agent = launch.agent.parse()?;
        let request = crate::cli::commands::queued_agent_request(&workspace, agent, launch.prompt)?;
        self.queue_add(request)
            .await?
            .entry
            .context("the daemon queued the agent without an entry")
    }
}
