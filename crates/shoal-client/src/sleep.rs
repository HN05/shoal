//! Whether Shoal wakes a space's agent for having stayed idle: an agent
//! that is waiting for something sleeps, until its next turn or for good.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    connection::{Daemon, request},
    protocol::AgentMethod,
    space_names::{SpaceName, SpaceSelector},
    state::Sleep,
};

/// A space's sleep after `shoal sleep`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slept {
    pub space: SpaceName,
    /// How long its agent sleeps; none while Shoal may wake it.
    pub sleep: Option<Sleep>,
    /// How many minutes its agent may stay idle before Shoal wakes it, as
    /// its mode sets; none when its mode never wakes it, which leaves a
    /// sleep unrecorded.
    pub idle_wake_minutes: Option<u64>,
}

/// Let the agent of `space` sleep as `sleep` says, or end its sleep with
/// none.
pub async fn set(
    daemon: &impl Daemon,
    space: SpaceSelector,
    sleep: Option<Sleep>,
) -> Result<Slept> {
    request(daemon, AgentMethod::Sleep { space, sleep }).await
}
