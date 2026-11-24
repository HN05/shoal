//! What `shoal restore` prints.
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;

use super::resume::Queued;
use crate::output::{batch::Failure, outputs};
use shoal_protocol::model::{QueueEntry, RemovedSpace, Space};

/// A space opened again from a removal record with its PRs linked, a
/// workspace set up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reopened {
    pub space: Space,
    pub removed: RemovedSpace,
    pub warnings: Vec<String>,
    /// Its stopped agents stay saved for a later `shoal resume`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub agents_saved: bool,
    /// The restores of its stopped agents, once queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<Queued>,
}

/// The stopped agents, last sessions, stopped commands and failed launches
/// `shoal restore --discard-agent` discarded.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscardedAgents {
    /// The executions of the stopped agents, last sessions and commands.
    pub discarded: Vec<ExecutionId>,
    /// The failed launches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub launches: Vec<QueueEntry>,
    /// The spaces it left, as those of an offline machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<super::agents::Skipped>,
    pub failed: Vec<Failure>,
}

/// What `shoal restore` without a space picks among: the removed spaces,
/// workspaces first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Restorable {
    pub removed: Vec<RemovedSpace>,
}

outputs!(Reopened, DiscardedAgents, Restorable);
