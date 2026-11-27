//! The preparation of migration 320. It moves the fields of each approval
//! request's record that queries read into the columns migration 319 made,
//! and drops the fields its row's columns held already.
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

/// The statuses a request had when this migration was written.
const STATUSES: &[&str] = &["pending", "approved", "denied", "outdated", "withdrawn"];

/// The states of the gate's step when this migration was written.
const GATE_STATES: &[&str] = &["deciding", "allowed", "unapplied", "unjudged", "escalated"];

/// The fields of a record that its row's columns held already: the key,
/// both numbers and their text, the target and name, the owner and whether
/// the request is active, and whether its space is gone, which reads
/// compute.
const HELD_BY_COLUMNS: &[&str] = &[
    "key",
    "space_number",
    "item",
    "number",
    "target",
    "name",
    "repository_id",
    "active",
    "space_removed",
];

/// A request's columns as the record held them, and what stays of it.
struct Moved {
    proposer: Option<String>,
    status: String,
    decider: Option<String>,
    gate: Option<String>,
    operation: Option<String>,
    record: Map<String, Value>,
}

/// A request's row before this migration.
struct Stored {
    rowid: i64,
    id: String,
    space: Option<String>,
    repository: Option<String>,
    target: String,
    name: String,
    record: String,
    active: i64,
    space_number: i64,
    number: i64,
}

pub(super) fn move_queried_fields(db: &Connection) -> Result<()> {
    for stored in stored_requests(db)? {
        let moved = moved(&stored.record, stored.repository.is_some())
            .with_context(|| format!("access_requests row {}", stored.id))?;
        insert_columns(db, &stored, &moved)?;
    }
    Ok(())
}

/// Every request's row, in the order they were made.
fn stored_requests(db: &Connection) -> Result<Vec<Stored>> {
    Ok(db
        .prepare(
            "SELECT rowid,id,space_id,repository_id,target_key,name,record,active,
                    space_number,number
                FROM access_requests ORDER BY rowid",
        )?
        .query_map([], |row| {
            Ok(Stored {
                rowid: row.get(0)?,
                id: row.get(1)?,
                space: row.get(2)?,
                repository: row.get(3)?,
                target: row.get(4)?,
                name: row.get(5)?,
                record: row.get(6)?,
                active: row.get(7)?,
                space_number: row.get(8)?,
                number: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Record `stored` with the columns `moved` took from its record, under
/// its row ID, so that the requests keep their order.
fn insert_columns(db: &Connection, stored: &Stored, moved: &Moved) -> Result<()> {
    db.execute(
        "INSERT INTO access_request_columns(rowid,id,space_id,repository_id,proposer_id,
                target_key,name,status,decider,gate,operation,record,active,space_number,
                number)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            stored.rowid,
            stored.id,
            stored.space,
            stored.repository,
            moved.proposer,
            stored.target,
            stored.name,
            moved.status,
            moved.decider,
            moved.gate,
            moved.operation,
            serde_json::to_string(&moved.record)?,
            stored.active,
            stored.space_number,
            stored.number
        ],
    )?;
    Ok(())
}

/// The columns `text`, a request's record, held, with the space that made
/// it as the proposer of a request its repository `owns`.
fn moved(text: &str, owns: bool) -> Result<Moved> {
    let Value::Object(mut record) = serde_json::from_str(text)? else {
        bail!("the record is no JSON object");
    };
    let space = text_field(&mut record, "space_id")?.context("the record names no space")?;
    let status = text_field(&mut record, "status")?.context("the record has no status")?;
    if !STATUSES.contains(&status.as_str()) {
        bail!("the record's status {status} is unknown");
    }
    let decider = text_field(&mut record, "decider")?;
    let operation = text_field(&mut record, "operation")?;
    let gate = gate_state(&mut record)?;
    for field in HELD_BY_COLUMNS {
        record.remove(*field);
    }
    Ok(Moved {
        proposer: owns.then_some(space),
        status,
        decider,
        gate,
        operation,
        record,
    })
}

/// Take the text of `field` out of `record`; none when it is absent or
/// null.
fn text_field(record: &mut Map<String, Value>, field: &str) -> Result<Option<String>> {
    match record.remove(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(other) => bail!("the record's {field} is {other}, not text"),
    }
}

/// Take the state of the gate's step out of `record`, leaving what else the
/// step says, unless it says nothing else.
fn gate_state(record: &mut Map<String, Value>) -> Result<Option<String>> {
    let mut gate = match record.remove("gate") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(gate)) => gate,
        Some(other) => bail!("the record's gate is {other}, not an object"),
    };
    let state = text_field(&mut gate, "state")?.context("the record's gate has no state")?;
    if !GATE_STATES.contains(&state.as_str()) {
        bail!("the record's gate state {state} is unknown");
    }
    if !gate.is_empty() {
        record.insert("gate".into(), Value::Object(gate));
    }
    Ok(Some(state))
}
