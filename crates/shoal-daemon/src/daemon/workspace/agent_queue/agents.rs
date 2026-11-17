//! Queuing the worker of an existing workspace in one request, as an app
//! and `shoal start` queue one in a ready workspace, or of a new one with
//! its record, to start once its preparation lets it.
use anyhow::{Context as _, Result};

use crate::{
    agents::resolve_launch,
    daemon::workspace::Manager,
    forge::items::reference::ItemInput,
    model::{Blocker, QueueEntry, Workspace},
    protocol::operations::{LaunchIssue, LaunchRequest, QueuedLaunch},
    start_plan::queued_agent_request,
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Queue the worker of the ready `workspace` that `launch` asks for,
    /// resolved under the workspace's settings and on its issue unless the
    /// launch gives it a task, once `blockers` are done and it can run
    /// there, linking `pr` as it starts.
    pub(crate) async fn queue_agent(
        &self,
        workspace: &SpaceSelector,
        launch: LaunchRequest,
        blockers: Vec<Blocker>,
        pr: Option<ItemInput>,
    ) -> Result<QueueEntry> {
        let workspace = self.workspace(workspace).await?;
        let settings = self.workspace_settings_with_templates(&workspace).await?;
        let mut launch = resolve_launch(launch, &settings)?;
        if let Some(issue) = assigned_issue(&workspace) {
            launch.default_issue(issue);
        }
        let mut request = queued_agent_request(&workspace, launch, pr)?;
        request.blockers = blockers;
        self.queue_add(request)
            .await?
            .entry
            .context("the daemon queued the agent without an entry")
    }

    /// Queue `launch` in the `workspace` a start opened, which waits until
    /// the workspace's preparation gets past its post-setup hook unless it
    /// is ready already, then starts as the queue admits it or, for one
    /// that starts now, at once regardless of the limits.
    pub(crate) async fn queue_for_preparation(
        &self,
        workspace: &Workspace,
        mut launch: QueuedLaunch,
    ) -> Result<QueueEntry> {
        let waits = workspace.state != WorkspaceState::Ready;
        let at_once = waits && std::mem::take(&mut launch.now);
        let request = queued_agent_request(workspace, launch, None)?;
        let entry = (self.queue_add(request).await?.entry)
            .context("the daemon queued the agent without an entry")?;
        if at_once {
            self.start_at_once_after_setup(&entry.id).await?;
        }
        Ok(entry)
    }
}

/// The first issue `workspace` is assigned, titled as its link recorded it,
/// else by its URL, without labels: the workspace ranks by its issues' own.
fn assigned_issue(workspace: &Workspace) -> Option<LaunchIssue> {
    let issue = workspace.links.issue()?;
    let url = issue.url.clone();
    let title = (issue.title.clone()).unwrap_or_else(|| url.to_string());
    let labels = Vec::new();
    Some(LaunchIssue { url, title, labels })
}
