//! The preparation of migration 303. It gives each journal record the one
//! shape its kind has: a creation no longer names the agent an earlier
//! Shoal's chat creations recorded, an event gives each field of its kind,
//! null when empty, where earlier Shoal versions left some out, and leaves
//! out the empty fields of its payload that its type leaves out.
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

/// The fields each kind with fields that may be empty gives, null when
/// they are.
const NULLABLE_FIELDS: &[(&str, &[&str])] = &[
    ("created", &["base_ref", "base_workspace", "parent"]),
    ("converted", &["handoff"]),
];

/// The fields a kind no longer records.
const DROPPED_FIELDS: &[(&str, &[&str])] = &[("created", &["agent"])];

/// The fields inside each kind's payload, by the payload's field, that it
/// leaves out when empty, where triggers wrote null.
const OMITTED_WHEN_NULL: &[(&str, &str, &[&str])] = &[
    ("file_added", "file", &["title", "session"]),
    ("file_removed", "file", &["title", "session"]),
    ("thread_posted", "thread", &["to"]),
    ("thread_resolved", "thread", &["outcome"]),
];

pub(super) fn rewrite_records(db: &Connection) -> Result<()> {
    let rows = {
        let mut reading = db.prepare(
            "SELECT id,record FROM space_events
                WHERE json_extract(record,'$.kind') IN ('created','converted',
                    'file_added','file_removed','thread_posted','thread_resolved')",
        )?;
        reading
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, text) in rows {
        let Ok(Value::Object(mut record)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if rewritten(&mut record) {
            db.execute(
                "UPDATE space_events SET record=?2 WHERE id=?1",
                params![id, serde_json::to_string(&record)?],
            )?;
        }
    }
    Ok(())
}

/// Give `record` its kind's shape; whether it lacked it.
fn rewritten(record: &mut Map<String, Value>) -> bool {
    let Some(kind) = record
        .get("kind")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return false;
    };
    let mut changed = false;
    for field in fields_of(DROPPED_FIELDS, &kind) {
        changed |= record.remove(*field).is_some();
    }
    for field in fields_of(NULLABLE_FIELDS, &kind) {
        if !record.contains_key(*field) {
            record.insert((*field).to_owned(), Value::Null);
            changed = true;
        }
    }
    for (named, payload, fields) in OMITTED_WHEN_NULL {
        if *named == kind
            && let Some(payload) = record.get_mut(*payload).and_then(Value::as_object_mut)
        {
            for field in *fields {
                if payload.get(*field).is_some_and(Value::is_null) {
                    payload.remove(*field);
                    changed = true;
                }
            }
        }
    }
    changed |= rename_thread_source(record);
    changed
}

/// Name a file a thread's post attached by the source `thread`, which an
/// earlier rename gave the file's record but not its events.
fn rename_thread_source(record: &mut Map<String, Value>) -> bool {
    let Some(source) = (record.get_mut("file"))
        .and_then(|file| file.get_mut("source"))
        .filter(|source| source.as_str() == Some("question"))
    else {
        return false;
    };
    *source = "thread".into();
    true
}

fn fields_of(table: &[(&str, &'static [&'static str])], kind: &str) -> &'static [&'static str] {
    table
        .iter()
        .find_map(|(named, fields)| (*named == kind).then_some(*fields))
        .unwrap_or_default()
}
