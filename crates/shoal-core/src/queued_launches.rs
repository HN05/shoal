//! Queuing a launch in an existing workspace, which the agent queue starts
//! detached: in a ready workspace, or in one whose setup failed, where it
//! waits for a retried setup.
use anyhow::{Context as _, Result};

use crate::client::space_names::SpaceSelector;
use crate::{
    agents::check_queued_launch,
    config::Effective,
    model::{QueueEntry, Workspace},
    paths::CommandPaths,
    protocol::{ConfigTarget, operations::QueuedLaunch},
    start_plan::queued_agent_request,
};

/// Queue `launch` in `workspace`, whose effective `settings` must let it
/// run, for the agent queue to start detached, linking `pr` as it starts.
pub async fn queue_in_workspace(
    paths: &CommandPaths,
    workspace: &Workspace,
    settings: &Effective,
    launch: QueuedLaunch,
    pr: Option<crate::forge::items::reference::ItemInput>,
) -> Result<QueueEntry> {
    check_queued_launch(&launch, settings)?;
    let request = queued_agent_request(workspace, launch, pr)?;
    let admission = crate::client::queue::add(paths, request).await?;
    admission
        .entry
        .context("the daemon queued the agent without an entry")
}

/// Queue `launch` in `workspace`, whose setup failed, to start once a
/// preparation there gets past its post-setup hook, then linking `pr`, the
/// PR its start was to link. It waits even when it was to start at once.
pub async fn queue_for_setup(
    paths: &CommandPaths,
    workspace: &Workspace,
    mut launch: QueuedLaunch,
    pr: Option<crate::forge::items::reference::ItemInput>,
) -> Result<QueueEntry> {
    launch.now = false;
    let target = ConfigTarget::Workspace(SpaceSelector::id(&workspace.id));
    let settings = crate::client::workspaces::settings(paths, target).await?;
    queue_in_workspace(paths, workspace, &settings, launch, pr).await
}
