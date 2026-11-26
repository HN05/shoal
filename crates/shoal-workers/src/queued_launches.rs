//! Queuing a launch in an existing workspace.
use anyhow::Result;

use crate::{
    client::connection::CommandPaths,
    model::{QueueEntry, Workspace},
    protocol::operations::QueuedLaunch,
};
use shoal_protocol::{queue::WorkerStart, space_names::SpaceSelector};

/// Queue `launch` in `workspace`, which the daemon checks under the
/// workspace's settings, for the agent queue to start detached, linking
/// `pr` as it starts.
pub async fn queue_in_workspace(
    paths: &CommandPaths,
    workspace: &Workspace,
    launch: QueuedLaunch,
    pr: Option<crate::forge::items::reference::ItemInput>,
) -> Result<QueueEntry> {
    let start = WorkerStart {
        workspace: SpaceSelector::id(&workspace.id),
        launch: launch.into(),
        blockers: Vec::new(),
        pr,
    };
    crate::client::queue::start_agent(paths, start).await
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
    queue_in_workspace(paths, workspace, launch, pr).await
}
