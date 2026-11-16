//! Setting a space's priority, which ranks its queued work.
use serde::{Deserialize, Serialize};
use shoal_schedule::priority::Priority;

use crate::space_names::SpaceName;

/// A space's priority after `shoal priority`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prioritized {
    pub space: SpaceName,
    pub priority: Priority,
}
