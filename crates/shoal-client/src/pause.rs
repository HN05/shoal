//! Pausing a machine, which then admits no new work and hands what it has
//! over, until it is unpaused.
use anyhow::Result;
use shoal_protocol::{
    pause::{Paused, Unpaused},
    protocol::MachineMethod,
};

use crate::connection::{Daemon, request};

/// Pause the joined machine `machine` names by its name or ID, or this one.
pub async fn pause(daemon: &impl Daemon, machine: Option<String>) -> Result<Paused> {
    request(daemon, MachineMethod::PauseMachine { machine }).await
}

/// Unpause the joined machine `machine` names by its name or ID, or this
/// one.
pub async fn unpause(daemon: &impl Daemon, machine: Option<String>) -> Result<Unpaused> {
    request(daemon, MachineMethod::UnpauseMachine { machine }).await
}
