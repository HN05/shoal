//! Setting a space's priority, which ranks its queued work.
use anyhow::Result;
use shoal_protocol::priority::Prioritized;

use shoal_schedule::priority::Priority;

use crate::connection::{Daemon, request};
use shoal_protocol::{protocol::QueueMethod, space_names::SpaceSelector};

/// Set `priority` on the space `space` names, which ranks its queued work.
pub async fn set(
    daemon: &impl Daemon,
    space: SpaceSelector,
    priority: Priority,
) -> Result<Prioritized> {
    request(daemon, QueueMethod::SetPriority { space, priority }).await
}
