//! The preparation of migration 314. It moves what a conversion's record
//! says it makes, `to`, and the preparation its agent waits for into the
//! columns of the table migration 313 made, and leaves the rest as the
//! record.
use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, params};
use serde_json::Value;

/// What a record's `to` names.
const TARGETS: &[&str] = &["workspace", "chat", "agent"];

pub(super) fn move_into_columns(db: &Connection) -> Result<()> {
    let rows = {
        let mut reading = db.prepare("SELECT space_id,record FROM space_conversions")?;
        reading
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, text) in rows {
        let (target, preparation, record) =
            split(&text).with_context(|| format!("space_conversions row {id}"))?;
        db.execute(
            "INSERT INTO space_conversions_columns(space_id,target,preparation,record)
             VALUES (?1,?2,?3,?4)",
            params![id, target, preparation, record],
        )?;
    }
    Ok(())
}

/// The record `text`'s target and preparation, and what remains of it.
fn split(text: &str) -> Result<(String, Option<String>, String)> {
    let Value::Object(mut record) = serde_json::from_str(text)? else {
        bail!("the record is not a JSON object");
    };
    let target = match record.remove("to") {
        Some(Value::String(target)) if TARGETS.contains(&target.as_str()) => target,
        other => bail!("the record's to is {other:?}, not one of {TARGETS:?}"),
    };
    let preparation = match record.remove("preparation") {
        None | Some(Value::Null) => None,
        Some(Value::String(preparation)) => Some(preparation),
        Some(other) => bail!("the record's preparation is {other}, not a string"),
    };
    Ok((target, preparation, Value::Object(record).to_string()))
}
