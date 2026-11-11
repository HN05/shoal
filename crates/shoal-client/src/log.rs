//! A space's log, the important events in the life of a workspace or chat,
//! and Shoal's, every space's with Shoal's own, as the daemon's event
//! journal holds them.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    connection::{self, Daemon},
    events::{ChatEvent, EventCursor, EventKind, FileEvent, ShoalEvent, WorkspaceEvent},
    forge::updates::UpdateKind,
    protocol::WorkspaceMethod,
    space_names::SpaceSelector,
    state::states,
    time::UnixTime,
};

states!(
    /// Which events a log shows.
    LogFilter {
        /// Its creation, renames, removal and restore; its agent's starts,
        /// stops and exits; its ready marks and completion; its issue and
        /// PR links, Shoal's changes to them and their closing, merging and
        /// reopening; its questions, approvals and alerts; and the spaces
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
    Event(Box<WorkspaceEvent>),
    Chat(Box<ChatEvent>),
    File(Box<FileEvent>),
    Shoal(Box<ShoalEvent>),
}

impl LogEntry {
    /// The journal ID, which orders a log's entries.
    pub fn id(&self) -> EventCursor {
        match self {
            Self::Event(event) => event.id,
            Self::Chat(event) => event.id,
            Self::File(event) => event.id,
            Self::Shoal(event) => event.id,
        }
    }

    pub fn created_at(&self) -> UnixTime {
        match self {
            Self::Event(event) => event.created_at,
            Self::Chat(event) => event.created_at,
            Self::File(event) => event.created_at,
            Self::Shoal(event) => event.created_at,
        }
    }

    /// Whether [`LogFilter::Entries`] shows the event: a chat's lifecycle,
    /// Shoal's events, and the workspace and chat events that filter names.
    pub fn is_entry(&self) -> bool {
        let Self::Event(event) = self else {
            return matches!(self, Self::Chat(_) | Self::Shoal(_));
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

/// The workspace and chat events the log shows, apart from forge activity.
const ENTRY_KINDS: [EventKind; 23] = [
    EventKind::Created,
    EventKind::Renamed,
    EventKind::Removed,
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
    EventKind::QuestionAsked,
    EventKind::QuestionAnswered,
    EventKind::ApprovalRequested,
    EventKind::ApprovalDecided,
    EventKind::AlertSent,
    EventKind::ChildCreated,
    EventKind::ChildRemoved,
    EventKind::JobEnded,
    // Recorded only by earlier versions, as `undone` records it now.
    EventKind::Continued,
];

/// The newest `limit` events of `space`'s log, or Shoal's without one, that
/// `filter` shows, oldest first. A removed space is named by its last name
/// until its events expire.
pub async fn log(
    daemon: &impl Daemon,
    space: Option<SpaceSelector>,
    filter: LogFilter,
    limit: u32,
) -> Result<Vec<LogEntry>> {
    let method = WorkspaceMethod::SpaceLog {
        space,
        filter,
        limit,
    };
    connection::pages(daemon, method).await
}

#[cfg(test)]
mod tests;
