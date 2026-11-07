//! Queuing an agent in an existing workspace in one request, as an app
//! queues one in a ready workspace.
use anyhow::{Context as _, Result};

use crate::client::space_names::SpaceSelector;
use crate::{
    agents::check_queued_launch, daemon::workspace::Manager, model::QueueEntry,
    protocol::operations::QueuedLaunch, start_plan::queued_agent_request,
};

impl Manager {
    /// Queue `agent` in the ready workspace `workspace`, with `prompt` as its
    /// first message, once it can run there.
    pub(crate) async fn queue_agent(
        &self,
        workspace: &SpaceSelector,
        agent: String,
        prompt: Option<String>,
    ) -> Result<QueueEntry> {
        let workspace = self.workspace(workspace).await?;
        let launch = QueuedLaunch {
            agent,
            prompt,
            ..Default::default()
        };
        check_queued_launch(&launch, &self.workspace_settings(&workspace).await?)?;
        let request = queued_agent_request(&workspace, launch)?;
        self.queue_add(request)
            .await?
            .entry
            .context("the daemon queued the agent without an entry")
    }
}
