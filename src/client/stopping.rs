//! Stopping what runs in a space: its agents and commands, which a stop
//! keeps to restore, and its untracked processes.
use anyhow::Result;

use crate::{
    client::{
        connection::{Daemon, request},
        processes::{Selection, Stopped},
    },
    model::{StoppedWorkspace, Workspace},
    protocol::{QueuedLaunches, RemovalMethod},
};

/// The workspaces with running agents or commands, or queued launches.
pub async fn targets(daemon: &impl Daemon) -> Result<Vec<Workspace>> {
    request(daemon, RemovalMethod::StopTargets).await
}

/// Refuse a stop of `workspace` the caller may not make.
pub async fn check(daemon: &impl Daemon, workspace: String) -> Result<()> {
    request(daemon, RemovalMethod::CheckStop { workspace }).await
}

/// Stop `workspace`'s agents and commands, keeping their sessions, and do
/// with its queued launches as `launches` says; `acknowledge_unproven`
/// accepts processes whose exit cannot be proven.
pub async fn stop_workspace(
    daemon: &impl Daemon,
    workspace: String,
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

/// Stop the untracked processes of `space` that `selection` selects, other
/// than the process `requester` and those that started it.
pub async fn stop_processes(
    daemon: &impl Daemon,
    space: String,
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
