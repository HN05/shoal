//! The preparation of migration 325. It gives each agent event the mode its
//! agent ran in: its execution's, while the store still keeps it, and
//! otherwise null.
use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

pub(super) fn record_modes(db: &Connection) -> Result<()> {
    let modes = execution_modes(db)?;
    let rows = {
        let mut reading = db.prepare(
            "SELECT id,record FROM space_events
                WHERE kind IN ('agent_started','agent_stopped','agent_exited')",
        )?;
        reading
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, text) in rows {
        let event = || format!("agent event {id}");
        let mut record = serde_json::from_str::<Map<String, Value>>(&text)
            .with_context(|| format!("{}: the record is no JSON object", event()))?;
        let agent = (record.get_mut("agent"))
            .and_then(Value::as_object_mut)
            .with_context(|| format!("{}: the record names no agent", event()))?;
        let execution = agent.get("execution_id").and_then(Value::as_str);
        let mode = execution.and_then(|execution| modes.get(execution).cloned().flatten());
        agent.insert("mode".into(), mode.map_or(Value::Null, Value::String));
        db.execute(
            "UPDATE space_events SET record=?2 WHERE id=?1",
            params![id, serde_json::to_string(&record)?],
        )?;
    }
    Ok(())
}

/// The mode each execution's agent started in, none where it records none.
fn execution_modes(db: &Connection) -> Result<HashMap<String, Option<String>>> {
    let mut reading = db.prepare("SELECT id,mode FROM executions")?;
    let modes = reading.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(modes.collect::<rusqlite::Result<_>>()?)
}
