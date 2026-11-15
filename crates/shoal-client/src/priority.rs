//! Setting a space's priority, which ranks its queued work.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use shoal_schedule::priority::Priority;

use crate::{
    connection::{Daemon, request},
    protocol::QueueMethod,
    space_names::{SpaceName, SpaceSelector},
};

/// A space's priority after `shoal priority`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prioritized {
    pub space: SpaceName,
    pub priority: Priority,
}

/// Set `priority` on the space `space` names, which ranks its queued work.
pub async fn set(
    daemon: &impl Daemon,
    space: SpaceSelector,
    priority: Priority,
) -> Result<Prioritized> {
    request(daemon, QueueMethod::SetPriority { space, priority }).await
}
