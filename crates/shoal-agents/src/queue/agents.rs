//! Queuing the agent of an existing workspace, or of a new space with its
//! record, in one request.
use anyhow::{Context as _, Result};
use shoal_protocol::progress::Blocker;

use super::store::Awaits;
use crate::Agents;
use crate::agents::resolve_launch;
use crate::daemon::Daemon;
use crate::forge::items::reference::ItemInput;
use crate::model::QueueAdmission;
use crate::model::QueueEntry;
use crate::model::Space;
use crate::model::Workspace;
use crate::protocol::operations::LaunchIssue;
use crate::protocol::operations::LaunchRequest;
use crate::protocol::operations::QueuedLaunch;
use crate::start_plan::queued_agent_request;
use shoal_protocol::space_names::SpaceSelector;

impl Agents {
    /// Queue the worker of the ready `workspace` that `launch` asks for,
    /// resolved under the workspace's settings and on its issue unless the
    /// launch gives it a task, once `blockers` are done and it can run
    /// there, linking `pr` as it starts.
    pub async fn queue_agent(
        &self,
        daemon: &dyn Daemon,
        workspace: &SpaceSelector,
        launch: LaunchRequest,
        blockers: Vec<Blocker>,
        pr: Option<ItemInput>,
    ) -> Result<QueueEntry> {
        let workspace = daemon.workspace(workspace).await?;
        let settings = daemon.workspace_settings_with_templates(&workspace).await?;
        let place = (crate::model::SpaceKind::Workspace, true);
        let mut launch = resolve_launch(launch, &settings, place)?;
        if let Some(issue) = assigned_issue(&workspace) {
            launch.default_issue(issue);
        }
        let mut request = queued_agent_request(&workspace, launch, pr);
        request.blockers = blockers;
        self.queue_add(daemon, request)
            .await?
            .entry
            .context("the daemon queued the agent without an entry")
    }

    /// Queue `launch` in the `space` a start recorded, linking `pr` as it
    /// starts, once `blockers` are done. Where it `awaits_preparation`, as in a
    /// new workspace, it waits until the preparation gets past the post-setup
    /// hook. One its caller runs `here` is left to the caller once admitted,
    /// and kept for it while it waits.
    pub async fn queue_start_agent(
        &self,
        daemon: &dyn Daemon,
        (space, awaits_preparation): (&Space, bool),
        mut launch: QueuedLaunch,
        (pr, blockers): (Option<ItemInput>, Vec<Blocker>),
    ) -> Result<QueueAdmission> {
        let awaits = match awaits_preparation {
            true => Awaits::Preparation {
                at_once: std::mem::take(&mut launch.now),
            },
            false => Awaits::FailedSetup,
        };
        let mut request = queued_agent_request(space, launch, pr);
        request.blockers = blockers;
        self.queue_add_awaiting(daemon, request, awaits).await
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
