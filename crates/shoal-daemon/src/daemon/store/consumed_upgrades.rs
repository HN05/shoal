//! The state that carried one upgrade each across it, which the daemon of
//! the release that made it consumed on its first start: the agents of
//! chats from before a chat's agent ran as an execution, the names chats
//! had before they were cut, the repositories whose files attached before
//! Shoal recorded them were still to be recorded. The migration that drops
//! it refuses a database that still has some, which only an earlier release
//! finishes.
use anyhow::{Result, ensure};
use rusqlite::Connection;

/// The last release whose daemon consumes this state.
const CONSUMING_RELEASE: &str = "v0.17.0";

/// The tables only an upgrade fills, which the daemon empties.
const TABLES: [&str; 3] = [
    "upgraded_chat_agents",
    "chat_legacy_names",
    "earlier_attached_files",
];

/// Refuse to migrate while a table that goes has rows, naming the table.
pub(super) fn refuse_unconsumed(db: &Connection) -> Result<()> {
    for table in TABLES {
        ensure!(
            !super::exists(db, &format!("SELECT 1 FROM {table}"), [])?,
            "the state database's {table} has rows that Shoal {CONSUMING_RELEASE} \
             consumes when its daemon starts; start Shoal {CONSUMING_RELEASE} once \
             before this version"
        );
    }
    Ok(())
}
