//! Whether Shoal wakes a space's agent for having stayed idle: an agent
//! that is waiting for something sleeps, until its next turn or for good.
use serde::{Deserialize, Serialize};

use crate::{space_names::SpaceName, state::Sleep};

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
