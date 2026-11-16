//! Whether Shoal wakes a space's agent for having stayed idle: an agent
//! that is waiting for something sleeps, until its next turn or for good.
use anyhow::Result;
use shoal_protocol::sleep::Slept;

use crate::connection::{Daemon, request};
use shoal_protocol::{protocol::AgentMethod, space_names::SpaceSelector, state::Sleep};

/// Let the agent of `space` sleep as `sleep` says, or with none end its
/// sleep and wake it.
pub async fn set(
    daemon: &impl Daemon,
    space: SpaceSelector,
    sleep: Option<Sleep>,
) -> Result<Slept> {
    request(daemon, AgentMethod::Sleep { space, sleep }).await
}
