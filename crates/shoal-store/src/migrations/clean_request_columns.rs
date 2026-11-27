//! The preparation of migration 327. It takes each clean request's status
//! out of its record into `moved_clean_requests`, from which the migration
//! fills the new table, and drops the workspace and request ID its columns
//! already hold.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

pub(super) fn move_fields(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TEMP TABLE moved_clean_requests(id INTEGER PRIMARY KEY, status TEXT NOT NULL,
            record TEXT NOT NULL)",
    )?;
    let rows = {
        let mut reading =
            db.prepare("SELECT id,request_id,workspace_id,record FROM simulator_clean_requests")?;
        reading
            .query_map([], |row| {
                Ok(Row {
                    id: row.get(0)?,
                    request_id: row.get(1)?,
                    workspace_id: row.get(2)?,
                    record: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for row in rows {
        let (status, record) = row
            .moved()
            .with_context(|| format!("simulator_clean_requests row {}", row.id))?;
        db.execute(
            "INSERT INTO temp.moved_clean_requests(id,status,record) VALUES (?1,?2,?3)",
            params![row.id, status, serde_json::to_string(&record)?],
        )?;
    }
    Ok(())
}

/// A clean request's row as the old table holds it.
struct Row {
    id: i64,
    request_id: String,
    workspace_id: String,
    record: String,
}

impl Row {
    /// The request's status and what remains of its record.
    fn moved(&self) -> Result<(String, Map<String, Value>)> {
        let Value::Object(mut record) = serde_json::from_str(&self.record)? else {
            bail!("the record is not a JSON object");
        };
        ensure!(
            record
                .remove("workspace_id")
                .as_ref()
                .and_then(Value::as_str)
                == Some(&*self.workspace_id),
            "the record names no workspace or another one"
        );
        let request_id = (record.get_mut("request"))
            .and_then(Value::as_object_mut)
            .and_then(|request| request.remove("request_id"));
        ensure!(
            request_id.as_ref().and_then(Value::as_str) == Some(&*self.request_id),
            "the record's request has no ID or another one"
        );
        let Some(Value::String(status)) = record.remove("status") else {
            bail!("the record names no status");
        };
        Ok((status, record))
    }
}
