//! Whether a space's agent is asleep: Shoal stopped it and queued its restore
//! on its own. The restore the daemon starts keeps why and what wakes it; a
//! waiting wrapper's restore reads both from the agent's recorded stop.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::stopped::{UNKNOWN_STOP, WAKES_IN_QUEUE};
use crate::model::Asleep;
use crate::state::QueueKind;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::space_ids::SpaceId;

/// Why the agent of the space `id` is asleep and what wakes it, while a
/// restore Shoal queued on its own waits for it.
pub fn current(db: &Connection, id: &SpaceId) -> Result<Option<Asleep>> {
    let found: Option<(Option<String>, Option<String>)> = db
        .prepare_cached(
            "SELECT COALESCE(q.stop_reason,s.reason),COALESCE(q.wakes_when,s.resumes_when)
             FROM agent_queue q LEFT JOIN agent_stops s ON s.execution_id=q.execution_id
             WHERE q.space_id=?1 AND q.kind=?2 AND q.automatic ORDER BY q.seq LIMIT 1",
        )?
        .query_row(params![id, QueueKind::Restore], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    Ok(found.map(|(reason, wakes_when)| Asleep {
        reason: reason.unwrap_or_else(|| UNKNOWN_STOP.to_owned()),
        wakes_when: wakes_when.unwrap_or_else(|| WAKES_IN_QUEUE.to_owned()),
        keeps: Vec::new(),
        held_by: None,
    }))
}

/// The entry of the restore [`current`] describes, with the execution it
/// restores, once it names one.
pub fn restore(db: &Connection, id: &SpaceId) -> Result<Option<(QueueEntryId, ExecutionId)>> {
    let found: Option<(QueueEntryId, Option<ExecutionId>)> = db
        .prepare_cached(
            "SELECT id,execution_id FROM agent_queue
             WHERE space_id=?1 AND kind=?2 AND automatic ORDER BY seq LIMIT 1",
        )?
        .query_row(params![id, QueueKind::Restore], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    Ok(found.and_then(|(entry, execution)| Some((entry, execution?))))
}

/// Wake the agent of the space `id` if it is asleep: the restores Shoal
/// queued on its own for it wait only for their turn in the agent queue, as
/// after `shoal resume`, without withdrawing a completion.
pub fn wake(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute(
        "UPDATE agent_queue SET automatic=0 WHERE space_id=?1 AND kind=?2 AND automatic",
        params![id, QueueKind::Restore],
    )?;
    Ok(())
}
