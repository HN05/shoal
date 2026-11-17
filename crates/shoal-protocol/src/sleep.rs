//! Whether Shoal wakes a space's agent for having stayed idle: an agent
//! that is waiting for something sleeps, until its next turn or for good,
//! and may set a timer that wakes it at a time.
use serde::{Deserialize, Serialize};

use crate::{space_names::SpaceName, state::Sleep, time::UnixTime};

/// A timer that sends a space's agent a message at a time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeTimer {
    /// When it fires.
    pub at: UnixTime,
    /// What the agent receives when it fires; without it, only that it
    /// fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A space's sleep after `shoal sleep`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slept {
    pub space: SpaceName,
    /// How long its agent sleeps; none once it is awake.
    pub sleep: Option<Sleep>,
    /// When its timer wakes its agent; none without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<UnixTime>,
    /// How many minutes its agent may stay idle before Shoal wakes it, as
    /// its mode sets; none when its mode never wakes it.
    pub idle_wake_minutes: Option<u64>,
}
