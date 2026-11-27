//! The preparation of migration 312. It moves the item's state out of each
//! record of what a space last saw of an item into the `state` column that
//! migration 311 added.
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

/// The tables that hold what a space's watch or poll last saw of an item.
const ACTIVITY_TABLES: &[&str] = &["pr_activity", "space_item_activity"];

/// The states a record holds.
const STATES: &[&str] = &[
    "open",
    "closed",
    "merged",
    "waiting",
    "running",
    "failed",
    "finished",
    "cancelled",
];

pub(super) fn move_states(db: &Connection) -> Result<()> {
    for table in ACTIVITY_TABLES {
        let rows = {
            let mut reading =
                db.prepare(&format!("SELECT rowid,space_id,url,record FROM {table}"))?;
            reading
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (rowid, space, url, text) in rows {
            let row = || format!("{table} row of space {space} and item {url}");
            let mut record = serde_json::from_str::<Map<String, Value>>(&text)
                .with_context(|| format!("{}: the record is no JSON object", row()))?;
            let state = take_state(&mut record).with_context(row)?;
            db.execute(
                &format!("UPDATE {table} SET state=?2,record=?3 WHERE rowid=?1"),
                params![rowid, state, serde_json::to_string(&record)?],
            )?;
        }
    }
    Ok(())
}

/// Take the state out of `record`: none before a first read, or for a
/// branch.
fn take_state(record: &mut Map<String, Value>) -> Result<Option<String>> {
    match record.remove("state") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(state)) if STATES.contains(&state.as_str()) => Ok(Some(state)),
        Some(other) => bail!("the record's state {other} is none an item takes"),
    }
}
