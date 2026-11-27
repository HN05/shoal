//! The rows of `simulators`: a simulator's state and owners in columns,
//! which the owner index covers, and its other fields in its record.
use anyhow::{Context, Result};
use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::acquirers::Acquirer;

use super::Simulator;
use crate::time::UnixTime;

const ALL: &str =
    "SELECT id,state,workspace_id,last_workspace_id,record FROM simulators ORDER BY id";
// Keep the expression identical to the schema index. An optional-parameter OR
// would prevent SQLite from seeking directly to the owner's records.
const OWNED: &str = "SELECT id,state,workspace_id,last_workspace_id,record FROM simulators
    WHERE coalesce(workspace_id, last_workspace_id) = ?1
    ORDER BY id";

/// A simulator's fields that its columns do not hold.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    udid: Option<String>,
    device: String,
    runtime: String,
    lease_name: Option<String>,
    reason: Option<String>,
    last_used: UnixTime,
    error: Option<String>,
    installed_apps: Option<usize>,
    acquirer: Acquirer,
}

pub(crate) fn list(db: &Connection, owner: Option<&SpaceId>) -> Result<Vec<Simulator>> {
    let mut statement = db.prepare(if owner.is_some() { OWNED } else { ALL })?;
    let mut rows = match owner {
        Some(owner) => statement.query([owner])?,
        None => statement.query([])?,
    };
    let mut simulators = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        #[cfg(test)]
        DECODED_RECORDS.with(|count| count.set(count.get() + 1));
        simulators
            .push(simulator(id.clone(), row).with_context(|| format!("decode simulator {id}"))?);
    }
    Ok(simulators)
}

/// The simulator `id` that `row`, as [`ALL`] selects it, holds.
fn simulator(id: String, row: &Row<'_>) -> Result<Simulator> {
    let record: Record = serde_json::from_str(&row.get::<_, String>(4)?)?;
    Ok(Simulator {
        id,
        udid: record.udid,
        device: record.device,
        runtime: record.runtime,
        workspace_id: row.get(2)?,
        last_workspace_id: row.get(3)?,
        lease_name: record.lease_name,
        reason: record.reason,
        state: row.get(1)?,
        last_used: record.last_used,
        error: record.error,
        installed_apps: record.installed_apps,
        acquirer: record.acquirer,
    })
}

/// Record `sim`, in place of the row of its ID.
pub(crate) fn save(db: &Connection, sim: &Simulator) -> Result<()> {
    let Simulator {
        id,
        udid,
        device,
        runtime,
        workspace_id,
        last_workspace_id,
        lease_name,
        reason,
        state,
        last_used,
        error,
        installed_apps,
        acquirer,
    } = sim.clone();
    let record = Record {
        udid,
        device,
        runtime,
        lease_name,
        reason,
        last_used,
        error,
        installed_apps,
        acquirer,
    };
    db.execute(
        "INSERT INTO simulators(id,state,workspace_id,last_workspace_id,record)
            VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(id) DO UPDATE SET state=excluded.state, workspace_id=excluded.workspace_id,
                last_workspace_id=excluded.last_workspace_id, record=excluded.record",
        params![
            id,
            state,
            workspace_id,
            last_workspace_id,
            serde_json::to_string(&record)?
        ],
    )?;
    Ok(())
}

pub(crate) fn delete(db: &Connection, id: &str) -> Result<()> {
    db.execute("DELETE FROM simulators WHERE id=?1", [id])?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    static DECODED_RECORDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
