//! Rewrite the JSON records the store holds into the shapes this release
//! reads, a migration per change of shape, so code reads one shape of each.
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};

/// Watches name when the space last changed the item `own_change_at`,
/// removal records keep every last session in `last_sessions`, and an
/// agent's terminal place is its `origin`.
pub(super) fn renamed_fields(db: &Connection) -> Result<()> {
    rewrite(db, "pr_activity", |watch| {
        renamed(watch, "own_state_at", "own_change_at")
    })?;
    rewrite(db, "removed_spaces", |removed| {
        if let Some(last) = removed
            .remove("last_session")
            .filter(|last| !last.is_null())
        {
            (removed.entry("last_sessions")).or_insert_with(|| Value::Array(vec![last]));
        }
        for agent in objects(removed, "agents") {
            if let Some(record) = agent.get_mut("record").and_then(Value::as_object_mut) {
                origin_from_place(record);
            }
        }
        for last in objects(removed, "last_sessions") {
            origin_from_place(last);
        }
    })
}

/// Fill what records an earlier Shoal wrote go without: a simulator's
/// acquirer, the user's; a stopped agent's mode, the default one; a last
/// session's execution, a new one; who attached a file, the user; and a
/// link event's relation, an assignment.
pub(super) fn filled_fields(db: &Connection) -> Result<()> {
    rewrite(db, "simulators", |simulator| {
        filled(simulator, "acquirer", || json!({"kind": "user"}))
    })?;
    rewrite(db, "removed_spaces", |removed| {
        for agent in objects(removed, "agents") {
            if let Some(record) = agent.get_mut("record").and_then(Value::as_object_mut) {
                filled(record, "mode", default_mode);
            }
        }
        for last in objects(removed, "last_sessions") {
            filled(last, "mode", default_mode);
            filled(last, "execution", || {
                json!(shoal_ids::execution_ids::ExecutionId::generate())
            });
        }
    })?;
    db.execute_batch(
        r#"UPDATE space_files SET attached_by='{"kind":"user"}' WHERE attached_by IS NULL;
        UPDATE space_events
            SET record=json_set(record,'$.file.attached_by',json('{"kind":"user"}'))
            WHERE json_valid(record) AND json_type(record,'$.file') IS NOT NULL
                AND coalesce(json_type(record,'$.file.attached_by'),'null')='null';
        UPDATE space_events SET record=json_set(record,'$.link.relation','assigned')
            WHERE json_valid(record) AND json_type(record,'$.link') IS NOT NULL
                AND json_type(record,'$.link.relation') IS NULL;"#,
    )?;
    Ok(())
}

fn default_mode() -> Value {
    json!(shoal_protocol::config::modes::ModeName::DEFAULT)
}

/// Rewrite every record of `table` with `change`, writing back those it
/// changed.
fn rewrite(db: &Connection, table: &str, change: impl Fn(&mut Map<String, Value>)) -> Result<()> {
    let records = db
        .prepare(&format!(
            "SELECT rowid,record FROM {table} WHERE json_valid(record)"
        ))?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (rowid, text) in records {
        let Value::Object(mut record) = serde_json::from_str(&text)? else {
            continue;
        };
        let before = record.clone();
        change(&mut record);
        if record != before {
            db.execute(
                &format!("UPDATE {table} SET record=?2 WHERE rowid=?1"),
                params![rowid, Value::Object(record).to_string()],
            )?;
        }
    }
    Ok(())
}

/// The objects of the array `record` holds under `key`.
fn objects<'a>(
    record: &'a mut Map<String, Value>,
    key: &str,
) -> impl Iterator<Item = &'a mut Map<String, Value>> {
    (record
        .get_mut(key)
        .and_then(Value::as_array_mut)
        .into_iter())
    .flatten()
    .filter_map(Value::as_object_mut)
}

/// Move `record`'s field `old` to `new`, unless it has `new` already.
fn renamed(record: &mut Map<String, Value>, old: &str, new: &str) {
    if let Some(value) = record.remove(old) {
        record.entry(new).or_insert(value);
    }
}

/// Give `record` the field `key`, as `value` makes it, unless it has one.
fn filled(record: &mut Map<String, Value>, key: &str, value: impl FnOnce() -> Value) {
    if record.get(key).is_none_or(Value::is_null) {
        record.insert(key.to_owned(), value());
    }
}

/// An agent's `place` as its `origin`, where a bare pane ID, which only
/// Herdr recorded, becomes a Herdr origin.
fn origin_from_place(record: &mut Map<String, Value>) {
    let Some(place) = record.remove("place") else {
        return;
    };
    let origin = match place {
        Value::String(pane) => json!({"host": "herdr", "place": pane}),
        origin => origin,
    };
    record.entry("origin").or_insert(origin);
}

#[cfg(test)]
mod tests;
