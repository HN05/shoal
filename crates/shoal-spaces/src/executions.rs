//! What the spaces read of the executions recorded in them.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};

use crate::{agent::Agent, state::ExecutionState};

/// Reads without decoding the records, so a damaged record still counts.
pub fn execution_count(
    db: &Connection,
    space_id: &SpaceId,
    state: Option<ExecutionState>,
) -> Result<usize> {
    let count: i64 = db
        .prepare_cached(
            "SELECT COUNT(*) FROM executions WHERE space_id=?1 AND (?2 IS NULL OR state=?2)",
        )?
        .query_row(rusqlite::params![space_id, state], |row| row.get(0))?;
    Ok(usize::try_from(count)?)
}

pub fn execution_state(
    db: &Connection,
    id: &ExecutionId,
) -> Result<Option<(SpaceId, ExecutionState)>> {
    Ok(db
        .query_row(
            "SELECT space_id,state FROM executions WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

pub fn newest_execution(db: &Connection, space_id: &SpaceId) -> Result<Option<ExecutionId>> {
    Ok(db
        .query_row(
            "SELECT id FROM executions WHERE space_id=?1 ORDER BY rowid DESC LIMIT 1",
            [space_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// The agent of the newest execution of the space that runs one.
pub fn newest_agent(db: &Connection, space_id: &SpaceId) -> Result<Option<Agent>> {
    Ok(db
        .query_row(
            "SELECT agent FROM executions WHERE space_id=?1 AND agent IS NOT NULL
             ORDER BY rowid DESC LIMIT 1",
            [space_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn spaces_with_executions(db: &Connection) -> Result<HashSet<SpaceId>> {
    Ok(db
        .prepare("SELECT DISTINCT space_id FROM executions")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
