//! The rows of a space's progress: its plan's status and steps as JSON,
//! and its blockers in a table of their own, which queries read.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};
use shoal_ids::space_ids::SpaceId;

use super::Plan;
use super::blockers::{StepRef, Stored};
use crate::time::UnixTime;
use shoal_protocol::progress::RecordedBlocker;

/// The plan with its blockers, and when it last changed.
pub(super) fn stored(db: &Connection, id: &SpaceId) -> Result<Option<(Plan, UnixTime)>> {
    let row: Option<(String, UnixTime)> = db
        .prepare_cached("SELECT record,updated_at FROM space_progress WHERE space_id=?1")?
        .query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((record, updated_at)) = row else {
        return Ok(None);
    };
    let mut plan: Plan = serde_json::from_str(&record)?;
    plan.blockers = db
        .prepare_cached(
            "SELECT kind,target,name,reason,lookup_failed,step_key,step_name
             FROM space_blockers WHERE space_id=?1 ORDER BY position",
        )?
        .query_map([id], blocker)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some((plan, updated_at)))
}

/// Replaces the plan, which changed at `now` and is yet to be published.
pub(super) fn write(db: &Connection, id: &SpaceId, plan: &Plan, now: UnixTime) -> Result<()> {
    db.execute(
        "INSERT INTO space_progress(space_id,record,updated_at) VALUES (?1,?2,?3)
         ON CONFLICT(space_id) DO UPDATE
         SET record=excluded.record,updated_at=excluded.updated_at,published_at=NULL",
        params![id, serde_json::to_string(plan)?, now],
    )?;
    db.execute("DELETE FROM space_blockers WHERE space_id=?1", [id])?;
    let mut inserting = db.prepare_cached(
        "INSERT INTO space_blockers(space_id,position,kind,target,name,reason,lookup_failed,
            step_key,step_name) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
    )?;
    for (position, stored) in (0_i64..).zip(&plan.blockers) {
        let RecordedBlocker {
            kind,
            target,
            name,
            reason,
            lookup_failed,
        } = &stored.blocker;
        let (key, step_name) = match &stored.step {
            Some(StepRef::Key(key)) => (Some(key), None),
            Some(StepRef::Name(name)) => (None, Some(name)),
            None => (None, None),
        };
        inserting.execute(params![
            id,
            position,
            kind,
            target,
            name,
            reason,
            lookup_failed,
            key,
            step_name
        ])?;
    }
    Ok(())
}

fn blocker(row: &Row<'_>) -> rusqlite::Result<Stored> {
    let step = match (row.get("step_key")?, row.get("step_name")?) {
        (Some(key), _) => Some(StepRef::Key(key)),
        (None, Some(name)) => Some(StepRef::Name(name)),
        (None, None) => None,
    };
    Ok(Stored {
        blocker: RecordedBlocker {
            kind: row.get("kind")?,
            target: row.get("target")?,
            name: row.get("name")?,
            reason: row.get("reason")?,
            lookup_failed: row.get("lookup_failed")?,
        },
        step,
    })
}

#[cfg(test)]
mod tests;
