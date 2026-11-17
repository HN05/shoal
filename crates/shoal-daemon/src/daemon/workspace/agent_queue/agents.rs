//! Queuing the worker of an existing workspace in one request, as an app
//! and `shoal start` queue one in a ready workspace, or of a new one with
//! its record, to start once its preparation lets it.
use anyhow::{Context as _, Result};

use super::store::Awaits;
use crate::{
    agents::resolve_launch,
    daemon::workspace::Manager,
    forge::items::reference::ItemInput,
    model::{Blocker, QueueEntry, Workspace},
    protocol::operations::{LaunchIssue, LaunchRequest, QueuedLaunch},
    start_plan::queued_agent_request,
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

    /// Queue `launch` in the `workspace` a start opened, linking `pr` as
    /// it starts. In a new workspace it waits until the preparation gets
    /// past the post-setup hook, then starts as the queue admits it or, for
    /// one that starts now, at once regardless of the limits; in a `reused`
    /// one, which runs no post-setup hook, it queues as in any ready one.
    pub(crate) async fn queue_for_preparation(
        &self,
        (workspace, reused): (&Workspace, bool),
        mut launch: QueuedLaunch,
        pr: Option<ItemInput>,
    ) -> Result<QueueEntry> {
        let awaits = match reused {
            true => Awaits::FailedSetup,
            false => Awaits::Preparation {
                at_once: std::mem::take(&mut launch.now),
            },
        };
        let request = queued_agent_request(workspace, launch, pr)?;
        (self.queue_add_awaiting(request, None, awaits).await?.entry)
            .context("the daemon queued the agent without an entry")
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
