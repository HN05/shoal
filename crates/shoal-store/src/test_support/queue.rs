//! The agent queue's rows a test reads where the queue offers no operation
//! that reads them.
use anyhow::Result;
use rusqlite::Connection;

/// The plans of the launches queued in chats, oldest first, while they wait
/// or start.
pub fn chat_launch_plans(db: &Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare(
            "SELECT q.plan FROM agent_queue q JOIN spaces s ON s.id=q.space_id
             WHERE q.kind='launch' AND s.kind='chat' ORDER BY q.seq",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The plan of the launch `id`, while it waits or starts.
pub fn launch_plan(db: &Connection, id: &str) -> Result<String> {
    Ok(
        db.query_row("SELECT plan FROM agent_queue WHERE id=?1", [id], |row| {
            row.get(0)
        })?,
    )
}

/// How many entries the queue records.
pub fn entry_count(db: &Connection) -> Result<usize> {
    let count: i64 = db.query_row("SELECT COUNT(*) FROM agent_queue", [], |row| row.get(0))?;
    Ok(usize::try_from(count)?)
}
