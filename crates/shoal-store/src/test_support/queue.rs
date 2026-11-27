//! The agent queue's rows a test reads where the queue offers no operation
//! that reads them.
use anyhow::Result;
use rusqlite::{Connection, Row};

/// What `read` reads of each launch queued in a chat, oldest first, while
/// it waits or starts.
pub fn chat_launches<T>(
    db: &Connection,
    read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    Ok(db
        .prepare(
            "SELECT q.* FROM agent_queue q JOIN spaces s ON s.id=q.space_id
             WHERE q.kind='launch' AND s.kind='chat' ORDER BY q.seq",
        )?
        .query_map([], read)?
        .collect::<rusqlite::Result<_>>()?)
}

/// What `read` reads of the queue entry `id`, while it waits or starts.
pub fn entry<T>(
    db: &Connection,
    id: &str,
    read: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<T> {
    Ok(db.query_row("SELECT * FROM agent_queue WHERE id=?1", [id], read)?)
}

/// How many entries the queue records.
pub fn entry_count(db: &Connection) -> Result<usize> {
    let count: i64 = db.query_row("SELECT COUNT(*) FROM agent_queue", [], |row| row.get(0))?;
    Ok(usize::try_from(count)?)
}
