//! Whether a space's agent is asleep: Shoal stopped it and queued its
//! restore on its own, so it comes back without `shoal restore`. The restore
//! the daemon starts keeps why and what wakes it; one a waiting wrapper
//! carries out reads both from the agent's recorded stop.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::stopped::{UNKNOWN_STOP, WAKES_IN_QUEUE};
use crate::{client::space_ids::SpaceId, model::Asleep, state::QueueKind};

/// Why the agent of the space `id` is asleep and what wakes it, while a
/// restore Shoal queued on its own waits for it.
pub(crate) fn current(db: &Connection, id: &SpaceId) -> Result<Option<Asleep>> {
    let found: Option<(Option<String>, Option<String>)> = db
        .query_row(
            "SELECT COALESCE(q.stop_reason,s.reason),COALESCE(q.wakes_when,s.resumes_when)
             FROM agent_queue q LEFT JOIN agent_stops s ON s.execution_id=q.execution_id
             WHERE q.space_id=?1 AND q.kind=?2 AND q.automatic ORDER BY q.seq LIMIT 1",
            params![id, QueueKind::Restore],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(found.map(|(reason, wakes_when)| Asleep {
        reason: reason.unwrap_or_else(|| UNKNOWN_STOP.to_owned()),
        wakes_when: wakes_when.unwrap_or_else(|| WAKES_IN_QUEUE.to_owned()),
    }))
}
