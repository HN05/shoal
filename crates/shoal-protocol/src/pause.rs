//! A machine's pause, through which it admits no new work and hands what
//! it has over, until its user unpauses it.
use serde::{Deserialize, Serialize};

use crate::time::UnixTime;

/// A pause the machine recorded, which then stops its work in the
/// background, an alert naming where each space went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paused {
    /// Since when the machine is paused, which a pause of a paused machine
    /// keeps.
    pub since: UnixTime,
}

/// What unpausing a machine did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unpaused {
    /// Since when it was paused; none when it was not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_since: Option<UnixTime>,
}
