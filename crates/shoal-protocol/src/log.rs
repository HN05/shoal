//! A space's log, the important events in the life of a workspace or chat,
//! and Shoal's, every space's with Shoal's own, as the daemon's event
//! journal holds them.
use serde::{Deserialize, Serialize};
use shoal_states::states;

use crate::{
    events::{EventCursor, EventKind, FileEvent, ShoalEvent, SpaceEvent},
    forge::updates::UpdateKind,
    space_names::SpaceSelector,
    time::UnixTime,
};

states!(
    /// Which events a log shows.
    LogFilter {
        /// Its creation, renames, removal and restore; its agent's starts,
        /// stops and exits; its ready marks and completion; its issue and
        /// PR links, Shoal's changes to them and their closing, merging and
        /// reopening; its threads, approvals and alerts; and the spaces
        /// created and removed below it; and Shoal's own events.
        Entries => "entries",
        /// Every event the journal holds for the log.
        All => "all",
    }
);

/// The entries of a space's log, or Shoal's without `space`, that a
/// subscription follows: the newest `limit` that `filter` shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogTopic {
    pub space: Option<SpaceSelector>,
    pub filter: LogFilter,
    pub limit: u32,
}

/// One event of a log, as `shoal internal events` prints it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogEntry {
    Event(Box<SpaceEvent>),
    File(Box<FileEvent>),
    Shoal(Box<ShoalEvent>),
}

impl LogEntry {
    /// The entry as a machine other than `machine`, which keeps the file
    /// it records, reads it.
    pub fn files_kept_on(&mut self, machine: &str) {
        if let Self::File(event) = self {
            event.details.file.copy.kept_on(machine);
        }
    }

    /// The journal ID, which orders a log's entries.
    pub fn id(&self) -> EventCursor {
        match self {
            Self::Event(event) => event.id,
            Self::File(event) => event.id,
            Self::Shoal(event) => event.id,
        }
    }

    pub fn created_at(&self) -> UnixTime {
        match self {
            Self::Event(event) => event.created_at,
            Self::File(event) => event.created_at,
            Self::Shoal(event) => event.created_at,
        }
    }

    /// Whether [`LogFilter::Entries`] shows the event: Shoal's events, and
    /// the events of spaces that filter names.
    pub fn is_entry(&self) -> bool {
        let Self::Event(event) = self else {
            return matches!(self, Self::Shoal(_));
        };
        let details = &event.details;
        match details.kind {
            EventKind::ItemActivity => details.item_activity.as_ref().is_some_and(|activity| {
                matches!(
                    activity.update.kind,
                    UpdateKind::Closed | UpdateKind::Merged | UpdateKind::Reopened
                )
            }),
            kind => ENTRY_KINDS.contains(&kind),
        }
    }
}

/// The events of spaces the log shows, apart from forge activity.
const ENTRY_KINDS: [EventKind; 34] = [
    EventKind::Created,
    EventKind::Renamed,
    EventKind::RemovalStarted,
    EventKind::Removed,
    EventKind::Retained,
    EventKind::Restored,
    EventKind::AgentStarted,
    EventKind::AgentStopped,
    EventKind::AgentExited,
    EventKind::ReviewReady,
    EventKind::ReviewCleared,
    EventKind::Completed,
    EventKind::Undone,
    EventKind::Linked,
    EventKind::Unlinked,
    EventKind::ItemChanged,
    EventKind::ThreadOpened,
    EventKind::ThreadPosted,
    EventKind::ThreadForwarded,
    EventKind::ThreadResolved,
    EventKind::ApprovalRequested,
    EventKind::ApprovalDecided,
    EventKind::GateDecided,
    EventKind::AlertSent,
    EventKind::ChildCreated,
    EventKind::ChildRemoved,
    EventKind::ParentChanged,
    EventKind::Absorbed,
    EventKind::AbsorbedSpace,
    EventKind::JobEnded,
    EventKind::NoMergeRuns,
    EventKind::BlockerAdded,
    EventKind::BlockerRemoved,
    EventKind::BlockerCleared,
];

#[cfg(test)]
mod tests;
