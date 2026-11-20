//! What links check of a space in their own transactions.
use anyhow::Result;
use rusqlite::Connection;
use shoal_ids::space_ids::SpaceId;
use shoal_links::daemon::Records;

/// The space records links read, which the daemon keeps.
pub(crate) struct LinkRecords;

impl Records for LinkRecords {
    fn ensure_accepts_links(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        super::ensure_accepts_links(db, id)
    }
}
