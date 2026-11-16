//! A space's log, the important events in the life of a workspace or chat,
//! and Shoal's, every space's with Shoal's own, as the daemon's event
//! journal holds them.
use anyhow::Result;
use shoal_protocol::log::{LogEntry, LogFilter};

use crate::connection::{self, Daemon};
use shoal_protocol::{protocol::WorkspaceMethod, space_names::SpaceSelector};

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
