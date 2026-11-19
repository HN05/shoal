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

/// The last release whose daemon looks for what earlier releases kept in
/// the state directory.
const STATE_RELEASE: &str = "v0.17.2";

/// What earlier releases kept in the state directory, which the daemon of
/// [`STATE_RELEASE`] still clears: each directory, what it holds, and what
/// clears it.
const EARLIER_STATE: [(&str, &str, &str); 3] = [
    (
        "unmoved",
        "packages of failed moves",
        "its daemon moves them to root_dir when it starts",
    ),
    (
        "reviews",
        "review runs",
        "its daemon removes them when it starts",
    ),
    (
        "proposals",
        "files proposals attach",
        "it removes them once their proposals are decided",
    ),
];

/// Refuse to drop what clears the files earlier releases kept in the state
/// directory, which may be the only copy of a chat or a worktree a
/// repository still lists, while some are still there, or while a notice
/// naming where moved packages went is unread.
pub(super) fn refuse_earlier_state(db: &Connection) -> Result<()> {
    if let Some(state) = super::state_directory(db) {
        for (directory, holds, clears) in EARLIER_STATE {
            ensure!(
                !state.join(directory).is_dir(),
                "the state directory holds {holds} in {directory}, which Shoal \
                 {STATE_RELEASE} clears: {clears}; run Shoal {STATE_RELEASE} until it \
                 is gone before this version"
            );
        }
    }
    ensure!(
        !super::exists(
            db,
            "SELECT 1 FROM notifications WHERE kind='unmoved_packages' AND read=0",
            [],
        )?,
        "an unread notice names packages of failed moves; read it with shoal inbox \
         under Shoal {STATE_RELEASE} before this version"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
