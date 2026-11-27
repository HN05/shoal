//! The preparation of migration 341. It moves the handle of each CI job that
//! a job's queue entry or a claimed job records out of the job's JSON into
//! the column migration 340 added.
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde_json::Value;

/// The tables whose rows record a job, with the rows that do.
const TABLES: &[(&str, &str)] = &[
    (
        "agent_queue",
        "SELECT id,job FROM agent_queue WHERE job IS NOT NULL",
    ),
    ("ci_jobs", "SELECT id,job FROM ci_jobs"),
];

pub(super) fn move_into_columns(db: &Connection) -> Result<()> {
    for (table, query) in TABLES {
        let rows = {
            let mut reading = db.prepare(query)?;
            let rows = reading.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<(String, String)>>>()?
        };
        for (id, job) in rows {
            let (handle, job) = split(&job).with_context(|| format!("{table} row {id}"))?;
            db.execute(
                &format!("UPDATE {table} SET handle=?2,job=?3 WHERE id=?1"),
                params![id, handle, job],
            )?;
        }
    }
    Ok(())
}

/// A job's handle, and the JSON of the job without it.
fn split(job: &str) -> Result<(String, String)> {
    let mut job: Value = serde_json::from_str(job).context("read its job")?;
    let Some(Value::Object(waiting)) = job.get_mut("job") else {
        bail!("its job names no waiting job");
    };
    let Some(Value::String(handle)) = waiting.remove("handle") else {
        bail!("its job has no handle");
    };
    Ok((handle, job.to_string()))
}
