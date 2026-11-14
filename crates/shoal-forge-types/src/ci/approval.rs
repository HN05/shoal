//! CI runs a forge holds until a maintainer approves them, as it does for
//! the PRs of forks and first-time contributors, and the runs an approval
//! reached.

use crate::items::reference::ItemRef;
use serde::{Deserialize, Serialize};

/// The runs an approval reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunsApproved {
    pub approved: Vec<String>,
}

/// A run the forge holds for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingRun {
    pub url: ItemRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
}
