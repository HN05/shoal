//! The policy that orders the queue's waiting work, and the running work
//! it stops to free resources. It owns the whole order, from priority to
//! the last tie-break; the daemon admits entries in the order it returns,
//! under limits of its own that no policy replaces.
use crate::{priority::Priority, rank::QueueRank, stopping::StopOrder};
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};

use super::snapshot::Snapshot;

/// An order of the queue's waiting work, read from a snapshot alone.
pub trait Scheduler {
    /// The waiting entries of `snapshot` in the order they start.
    fn rank(&self, snapshot: &Snapshot) -> Vec<Ranked>;

    /// The spaces `ids` in the order their agents would start, first first,
    /// each with its priority, which puts the last of them to sleep first
    /// to make room.
    fn spaces_in_order(&self, snapshot: &Snapshot, ids: &[SpaceId]) -> Vec<(SpaceId, Priority)>;

    /// The snapshot's running work in the order it stops to free
    /// resources, first first.
    fn stop_order(&self, snapshot: &Snapshot) -> StopOrder;
}

/// A waiting entry with its rank, in queue order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub id: QueueEntryId,
    pub rank: QueueRank,
}
