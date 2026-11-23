//! A machine's pause, through which it admits no new work and hands what
//! it has over, until its user unpauses it.
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::SpaceNumber;

use crate::time::UnixTime;

/// What pausing a machine did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paused {
    /// Since when the machine is paused, which a pause of a paused machine
    /// keeps.
    pub since: UnixTime,
    /// The spaces whose agents the pause stopped there, whether they went
    /// to another machine or stayed, as the pause's alert says.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped: Vec<SpaceNumber>,
}

/// What unpausing a machine did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unpaused {
    /// Since when it was paused; none when it was not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_since: Option<UnixTime>,
}
