//! Reading a space's log, or Shoal's.
use anyhow::Result;
use shoal_protocol::log::{LogEntry, LogFilter};

use crate::connection::{self, Daemon};
use shoal_protocol::{protocol::SpaceMethod, space_names::SpaceSelector};

/// The newest `limit` events of `space`'s log, or Shoal's without one, that
/// `filter` shows, oldest first. A removed space is named by its last name
/// until its events expire.
pub async fn log(
    daemon: &impl Daemon,
    space: Option<SpaceSelector>,
    filter: LogFilter,
    limit: u32,
) -> Result<Vec<LogEntry>> {
    let method = SpaceMethod::SpaceLog {
        space,
        filter,
        limit,
    };
    connection::pages(daemon, method).await
}
