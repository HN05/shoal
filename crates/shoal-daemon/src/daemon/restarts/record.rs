//! The store side of the restarts the daemon took for later: one per space,
//! with the execution of the agent it is for, how that agent restarts and
//! who asked, and a generation each record
//! takes anew, which tells a newer request's record from the one it
//! replaced. One the daemon began stays until the agent's restore applies
//! its options, which each wrapper that reattaches meanwhile reads.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};

use crate::{model::AgentRestart, time::UnixTime};

/// A restart the daemon took for later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recorded {
    pub space_id: SpaceId,
    /// The execution the agent ran as when the restart was asked for, which
    /// one a restart runs it as later followed.
    pub execution: ExecutionId,
    pub restart: AgentRestart,
    /// The space whose process asked for it; none for the user.
    pub asked_by: Option<SpaceId>,
    /// The daemon began it, so it goes on until the agent's restore
    /// applies its options or the agent goes.
    pub begun: bool,
}

/// Record `recorded`, not begun, replacing what the space had.
pub(super) fn put(db: &Connection, recorded: &Recorded) -> Result<()> {
    db.execute(
        "INSERT INTO agent_restarts(space_id,restart,asked_by,recorded_at,generation,execution)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(space_id) DO UPDATE SET restart=excluded.restart,asked_by=excluded.asked_by,
            generation=excluded.generation,execution=excluded.execution,begun=0",
        params![
            recorded.space_id,
            serde_json::to_string(&recorded.restart)?,
            recorded.asked_by,
            UnixTime::now(),
            uuid::Uuid::new_v4().to_string(),
            recorded.execution,
        ],
    )?;
    Ok(())
}

/// The restart recorded for the space `id`, which it no longer has.
pub(super) fn take(db: &Connection, id: &SpaceId) -> Result<Option<Recorded>> {
    let recorded = find(db, id)?;
    db.execute("DELETE FROM agent_restarts WHERE space_id=?1", [id])?;
    Ok(recorded)
}

/// The restart recorded for the space `id`, with its generation, if it has
/// one.
pub(super) fn find_generation(db: &Connection, id: &SpaceId) -> Result<Option<(Recorded, String)>> {
    let Some(recorded) = find(db, id)? else {
        return Ok(None);
    };
    let generation = db.query_row(
        "SELECT generation FROM agent_restarts WHERE space_id=?1",
        [id],
        |row| row.get(0),
    )?;
    Ok(Some((recorded, generation)))
}

/// Forget the restart recorded for the space `id` if it is still of
/// `generation`, which a newer request's record replaces.
pub(super) fn take_generation(db: &Connection, id: &SpaceId, generation: &str) -> Result<()> {
    db.execute(
        "DELETE FROM agent_restarts WHERE space_id=?1 AND generation=?2",
        params![id, generation],
    )?;
    Ok(())
}

/// Mark the restart of `generation` recorded for the space `id` as begun,
/// which a wrapper that reattaches to the next daemon continues.
pub(super) fn begin(db: &Connection, id: &SpaceId, generation: &str) -> Result<()> {
    db.execute(
        "UPDATE agent_restarts SET begun=1 WHERE space_id=?1 AND generation=?2",
        params![id, generation],
    )?;
    Ok(())
}

/// The begun restart recorded for the agent that runs as `execution`,
/// which its wrapper continues.
pub(crate) fn begun(db: &Connection, execution: &ExecutionId) -> Result<Option<AgentRestart>> {
    let restart: Option<String> = db
        .query_row(
            "SELECT restart FROM agent_restarts WHERE execution=?1 AND begun=1",
            [execution],
            |row| row.get(0),
        )
        .optional()?;
    Ok(restart
        .map(|restart| serde_json::from_str(&restart))
        .transpose()?)
}

/// Forget the begun restart recorded for the agent that runs as
/// `execution`, whose restore applied its options.
pub(crate) fn applied(db: &Connection, execution: &ExecutionId) -> Result<()> {
    db.execute(
        "DELETE FROM agent_restarts WHERE execution=?1 AND begun=1",
        [execution],
    )?;
    Ok(())
}

/// Let the restart begun for the agent that runs as `execution` begin
/// again, as its wrapper still runs the agent it never stopped.
pub(crate) fn undelivered(db: &Connection, execution: &ExecutionId) -> Result<()> {
    db.execute(
        "UPDATE agent_restarts SET begun=0 WHERE execution=?1",
        [execution],
    )?;
    Ok(())
}

/// Follow the agent that ran as `from` to `to`, the execution a new
/// wrapper runs it as after a restart, whose registration applied the
/// options of the restart begun for it.
pub(crate) fn follow(db: &Connection, from: &ExecutionId, to: &ExecutionId) -> Result<()> {
    applied(db, from)?;
    db.execute(
        "UPDATE agent_restarts SET execution=?2 WHERE execution=?1",
        params![from, to],
    )?;
    Ok(())
}

/// How the agent of the space `id` restarts once the daemon acts on the
/// restart it recorded, if it did.
pub(crate) fn current(db: &Connection, id: &SpaceId) -> Result<Option<AgentRestart>> {
    Ok(find(db, id)?.map(|recorded| recorded.restart))
}

/// Every recorded restart, in the order they were recorded.
pub(super) fn all(db: &Connection) -> Result<Vec<Recorded>> {
    let rows = db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM agent_restarts ORDER BY recorded_at,rowid"
        ))?
        .query_map([], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(recorded).collect()
}

/// The restart recorded for the space `id`, if it has one.
pub(super) fn find(db: &Connection, id: &SpaceId) -> Result<Option<Recorded>> {
    let query = format!("SELECT {COLUMNS} FROM agent_restarts WHERE space_id=?1");
    let found = db.query_row(&query, [id], row).optional()?;
    found.map(recorded).transpose()
}

const COLUMNS: &str = "space_id,execution,restart,asked_by,begun";

/// A row of [`COLUMNS`], with the restart as its JSON.
type Row = (SpaceId, ExecutionId, String, Option<SpaceId>, bool);

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn recorded((space_id, execution, restart, asked_by, begun): Row) -> Result<Recorded> {
    Ok(Recorded {
        space_id,
        execution,
        restart: serde_json::from_str(&restart)?,
        asked_by,
        begun,
    })
}
