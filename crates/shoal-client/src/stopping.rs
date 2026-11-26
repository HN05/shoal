//! Stopping what runs in a space.
use anyhow::Result;

use crate::connection::{Daemon, Prepared, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    model::{Space, StoppedSpace},
    processes::{Selection, Stopped},
    protocol::{QueuedLaunches, RemovalMethod, UntrackedProcesses},
};

/// The workspaces with running agents or commands, or queued launches,
/// and the running chats.
pub async fn targets(daemon: &impl Daemon) -> Result<Vec<Space>> {
    request(daemon, RemovalMethod::StopTargets).await
}

/// Refuse a stop of the workspace or chat `space` the caller may not make.
pub fn check(space: SpaceSelector) -> Prepared<()> {
    Prepared::new(RemovalMethod::CheckStop { space })
}

/// Stop the agents and commands of `space`, a workspace or a chat, keeping
/// their sessions, and do with its queued launches as `launches` says and
/// with its untracked processes as `processes` says; `acknowledge_unproven`
/// accepts processes whose exit cannot be proven.
pub fn stop_space(
    space: SpaceSelector,
    acknowledge_unproven: bool,
    launches: QueuedLaunches,
    processes: UntrackedProcesses,
) -> Prepared<StoppedSpace> {
    Prepared::new(RemovalMethod::StopSpace {
        space,
        acknowledge_unproven,
        launches,
        processes,
    })
}

/// Stop the untracked processes of `space` that `selection` selects, other
/// than the process `requester` and those that started it.
pub fn stop_processes(
    space: SpaceSelector,
    selection: Selection,
    requester: Option<u32>,
) -> Prepared<Stopped> {
    Prepared::new(RemovalMethod::StopProcesses {
        space,
        selection,
        requester,
    })
}
