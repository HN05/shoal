//! The interface of a policy that orders the agent queue. A policy owns the
//! whole order, down to the last tie-break.
use crate::{priority::Priority, rank::QueueRank, stopping::StopOrder};
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};

use super::snapshot::Snapshot;

pub trait Scheduler {
    /// The waiting entries of `snapshot`, first to start first.
    fn rank(&self, snapshot: &Snapshot) -> Vec<Ranked>;

    /// `ids` in the order their agents would start, first first, each with
    /// its priority.
    fn spaces_in_order(&self, snapshot: &Snapshot, ids: &[SpaceId]) -> Vec<(SpaceId, Priority)>;

    fn stop_order(&self, snapshot: &Snapshot) -> StopOrder;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub id: QueueEntryId,
    /// The entry's name in `shoal queue`.
    pub name: String,
    pub rank: QueueRank,
}
