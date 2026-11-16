//! Subscriptions: a snapshot of the records of some topics, then their
//! changes, on a connection of their own. A daemon restart closes the
//! connection; the subscription subscribes again and reports the new
//! snapshot as a resync.
pub mod records;

use records::{Record, RecordKey};

/// What a subscription reports.
#[derive(Debug, Clone)]
pub enum Update {
    /// Every record the subscription covers, as it is now. `resynced` marks
    /// a snapshot after a reconnect, which replaces everything known before:
    /// a record it lacks was removed meanwhile.
    Snapshot {
        records: Vec<Record>,
        resynced: bool,
    },
    /// A record was added or changed.
    Changed(Box<Record>),
    /// A record was removed.
    Removed(RecordKey),
}
