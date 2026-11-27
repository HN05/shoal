//! The preparation of migration 322. It splits each resolved thread's
//! outcome into its kind, which stays in `outcome`, the cause of a
//! closing, and what else it says, in the columns migration 321 made.
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

/// The kinds of outcome when this migration was written.
const KINDS: &[&str] = &[
    "answered",
    "done",
    "fixed",
    "false-positive",
    "wont-fix",
    "deferred",
    "duplicate",
    "withdrawn",
    "closed",
];

/// The causes of a closing when this migration was written.
const CAUSES: &[&str] = &["completion", "removal"];

/// The fields an outcome held beside its kind and cause.
const DETAILS: &[&str] = &["reason", "issue", "of"];

/// An outcome's columns: its kind, the cause of a closing, and what else it
/// says as JSON.
type Columns = (String, Option<String>, Option<String>);

pub(super) fn split_outcomes(db: &Connection) -> Result<()> {
    for (id, outcome) in resolved(db)? {
        let columns = columns(&outcome).with_context(|| format!("threads row {id}"))?;
        write_columns(db, id, columns)?;
    }
    Ok(())
}

/// Each resolved thread's ID and outcome as JSON.
fn resolved(db: &Connection) -> Result<Vec<(i64, String)>> {
    Ok(db
        .prepare("SELECT id,outcome FROM threads WHERE outcome IS NOT NULL")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Store the outcome of the thread `id` as `columns`.
fn write_columns(db: &Connection, id: i64, (kind, cause, details): Columns) -> Result<()> {
    db.execute(
        "UPDATE threads SET outcome=?2,cause=?3,outcome_details=?4 WHERE id=?1",
        params![id, kind, cause, details],
    )?;
    Ok(())
}

/// The columns of `text`, an outcome as JSON.
fn columns(text: &str) -> Result<Columns> {
    let Value::Object(mut outcome) = serde_json::from_str(text)? else {
        bail!("the outcome is no JSON object");
    };
    let kind = text_field(&mut outcome, "outcome")?.context("the outcome has no kind")?;
    if !KINDS.contains(&kind.as_str()) {
        bail!("the outcome's kind {kind} is unknown");
    }
    let cause = text_field(&mut outcome, "cause")?;
    match (&cause, kind == "closed") {
        (None, false) => {}
        (Some(cause), true) if CAUSES.contains(&cause.as_str()) => {}
        (None, true) => bail!("the closed outcome has no cause"),
        (Some(cause), _) => bail!("the {kind} outcome's cause {cause} does not fit it"),
    }
    if let Some(field) = outcome
        .keys()
        .find(|field| !DETAILS.contains(&field.as_str()))
    {
        bail!("the outcome's field {field} is unknown");
    }
    let details = match outcome.is_empty() {
        true => None,
        false => Some(serde_json::to_string(&outcome)?),
    };
    Ok((kind, cause, details))
}

/// Take the text of `field` out of `outcome`; none when it is absent.
fn text_field(outcome: &mut Map<String, Value>, field: &str) -> Result<Option<String>> {
    match outcome.remove(field) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(other) => bail!("the outcome's {field} is {other}, not text"),
    }
}
