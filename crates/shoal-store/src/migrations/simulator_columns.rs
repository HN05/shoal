//! The preparation of migration 326. It takes each simulator's state and
//! owners out of its record, with the ID its column already holds, into
//! `moved_simulators`, from which the migration fills the new table.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

pub(super) fn move_fields(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TEMP TABLE moved_simulators(id TEXT PRIMARY KEY, state TEXT NOT NULL,
            workspace_id TEXT, last_workspace_id TEXT, record TEXT NOT NULL)",
    )?;
    let rows = {
        let mut reading = db.prepare("SELECT id,record FROM simulators")?;
        reading
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, text) in rows {
        let moved = moved(&id, &text).with_context(|| format!("simulators row {id}"))?;
        db.execute(
            "INSERT INTO temp.moved_simulators(id,state,workspace_id,last_workspace_id,record)
                VALUES (?1,?2,?3,?4,?5)",
            params![
                id,
                moved.state,
                moved.workspace_id,
                moved.last_workspace_id,
                serde_json::to_string(&moved.record)?
            ],
        )?;
    }
    Ok(())
}

/// A simulator's row as the new table holds it.
struct Moved {
    state: String,
    workspace_id: Option<String>,
    last_workspace_id: Option<String>,
    record: Map<String, Value>,
}

/// The row of the simulator `id` whose record is `text`.
fn moved(id: &str, text: &str) -> Result<Moved> {
    let Value::Object(mut record) = serde_json::from_str(text)? else {
        bail!("the record is not a JSON object");
    };
    ensure!(
        record.remove("id").as_ref().and_then(Value::as_str) == Some(id),
        "the record names no ID or another one"
    );
    let Some(Value::String(state)) = record.remove("state") else {
        bail!("the record names no state");
    };
    Ok(Moved {
        state,
        workspace_id: owner(&mut record, "workspace_id")?,
        last_workspace_id: owner(&mut record, "last_workspace_id")?,
        record,
    })
}

/// The workspace `record` names by `field`, which it may leave out.
fn owner(record: &mut Map<String, Value>, field: &str) -> Result<Option<String>> {
    match record.remove(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(owner)) => Ok(Some(owner)),
        Some(other) => bail!("the record's {field} is {other}, not a workspace ID"),
    }
}
