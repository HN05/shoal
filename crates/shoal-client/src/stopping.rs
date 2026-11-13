//! Stopping what runs in a space: its agents and commands, which a stop
//! keeps to restore, and its untracked processes.
use anyhow::Result;

use crate::space_names::SpaceSelector;
use crate::{
    connection::{Daemon, request},
    model::{PresentSpace, StoppedSpace},
    processes::{Selection, Stopped},
    protocol::{QueuedLaunches, RemovalMethod, UntrackedProcesses},
};

/// The workspaces with running agents or commands, or queued launches,
/// and the running chats.
pub async fn targets(daemon: &impl Daemon) -> Result<Vec<PresentSpace>> {
    request(daemon, RemovalMethod::StopTargets).await
}

/// Refuse a stop of the workspace or chat `space` the caller may not make.
pub async fn check(daemon: &impl Daemon, space: SpaceSelector) -> Result<()> {
    request(daemon, RemovalMethod::CheckStop { space }).await
}

/// Stop the agents and commands of `space`, a workspace or a chat, keeping
/// their sessions, and do with its queued launches as `launches` says and
/// with its untracked processes as `processes` says; `acknowledge_unproven`
/// accepts processes whose exit cannot be proven.
pub async fn stop_space(
    daemon: &impl Daemon,
    space: SpaceSelector,
    acknowledge_unproven: bool,
    launches: QueuedLaunches,
    processes: UntrackedProcesses,
) -> Result<StoppedSpace> {
    let method = RemovalMethod::StopSpace {
        space,
        acknowledge_unproven,
        launches,
        processes,
    };
    request(daemon, method).await
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
