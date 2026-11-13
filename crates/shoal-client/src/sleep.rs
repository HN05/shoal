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
    /// How long its agent sleeps; none once it is awake.
    pub sleep: Option<Sleep>,
    /// How many minutes its agent may stay idle before Shoal wakes it, as
    /// its mode sets; none when its mode never wakes it.
    pub idle_wake_minutes: Option<u64>,
}

/// Let the agent of `space` sleep as `sleep` says, or with none end its
/// sleep and wake it.
pub async fn set(
    daemon: &impl Daemon,
    space: SpaceSelector,
    sleep: Option<Sleep>,
) -> Result<Slept> {
    request(daemon, AgentMethod::Sleep { space, sleep }).await
}
