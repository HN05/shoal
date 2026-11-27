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
