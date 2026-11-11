//! Stopping what runs in a space: its agents and commands, or a chat's
//! agent, which a stop keeps to restore, and its untracked processes.
use anyhow::Result;

use crate::space_names::SpaceSelector;
use crate::{
    connection::{Daemon, request},
    model::{StoppedChat, StoppedWorkspace, Workspace},
    processes::{Selection, Stopped},
    protocol::{QueuedLaunches, RemovalMethod},
};

/// The workspaces with running agents or commands, or queued launches.
pub async fn targets(daemon: &impl Daemon) -> Result<Vec<Workspace>> {
    request(daemon, RemovalMethod::StopTargets).await
}

/// Refuse a stop of the workspace or chat `space` the caller may not make.
pub async fn check(daemon: &impl Daemon, space: SpaceSelector) -> Result<()> {
    request(daemon, RemovalMethod::CheckStop { space }).await
}

/// Stop `workspace`'s agents and commands, keeping their sessions, and do
/// with its queued launches as `launches` says; `acknowledge_unproven`
/// accepts processes whose exit cannot be proven.
pub async fn stop_workspace(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    acknowledge_unproven: bool,
    launches: QueuedLaunches,
) -> Result<StoppedWorkspace> {
    let method = RemovalMethod::StopWorkspace {
        workspace,
        acknowledge_unproven,
        launches,
    };
    request(daemon, method).await
}

/// Save `chat` for `shoal resume` and stop its agent, as the caller's stop.
pub async fn stop_chat(daemon: &impl Daemon, chat: SpaceSelector) -> Result<StoppedChat> {
    request(daemon, RemovalMethod::StopChat { chat }).await
}

/// Stop the untracked processes of `space` that `selection` selects, other
/// than the process `requester` and those that started it.
pub async fn stop_processes(
    daemon: &impl Daemon,
    space: SpaceSelector,
    selection: Selection,
    requester: Option<u32>,
) -> Result<Stopped> {
    let method = RemovalMethod::StopProcesses {
        space,
        selection,
        requester,
    };
    request(daemon, method).await
}
