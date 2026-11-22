//! Setting a space's priority, which ranks its queued work.
use shoal_protocol::priority::Prioritized;

use shoal_schedule::priority::Priority;

use crate::connection::Prepared;
use shoal_protocol::{protocol::QueueMethod, space_names::SpaceSelector};

/// Set `priority` on the space `space` names, which ranks its queued work.
pub fn set(space: SpaceSelector, priority: Priority) -> Prepared<Prioritized> {
    Prepared::new(QueueMethod::SetPriority { space, priority })
}
