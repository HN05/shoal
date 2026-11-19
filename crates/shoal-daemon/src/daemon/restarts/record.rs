//! The store side of the restarts the daemon took for later: one per space,
//! with how its agent restarts and who asked.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;

use crate::{model::AgentRestart, time::UnixTime};

/// A restart the daemon took for later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recorded {
    pub space_id: SpaceId,
    pub restart: AgentRestart,
    /// The space whose process asked for it; none for the user.
    pub asked_by: Option<SpaceId>,
}

/// Record `recorded`, replacing what the space had.
pub(super) fn put(db: &Connection, recorded: &Recorded) -> Result<()> {
    db.execute(
        "INSERT INTO agent_restarts(space_id,restart,asked_by,recorded_at) VALUES (?1,?2,?3,?4)
         ON CONFLICT(space_id) DO UPDATE SET restart=excluded.restart,asked_by=excluded.asked_by",
        params![
            recorded.space_id,
            serde_json::to_string(&recorded.restart)?,
            recorded.asked_by,
            UnixTime::now(),
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

/// Forget `recorded` unless a newer restart replaced it.
pub(super) fn take_same(db: &Connection, recorded: &Recorded) -> Result<()> {
    db.execute(
        "DELETE FROM agent_restarts WHERE space_id=?1 AND restart=?2 AND asked_by IS ?3",
        params![
            recorded.space_id,
            serde_json::to_string(&recorded.restart)?,
            recorded.asked_by,
        ],
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

const COLUMNS: &str = "space_id,restart,asked_by";

/// A row of [`COLUMNS`], with the restart as its JSON.
type Row = (SpaceId, String, Option<SpaceId>);

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}

fn recorded((space_id, restart, asked_by): Row) -> Result<Recorded> {
    Ok(Recorded {
        space_id,
        restart: serde_json::from_str(&restart)?,
        asked_by,
    })
}
