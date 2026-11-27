//! Migration 326: a simulator's state and owners move into columns.
use super::*;
use serde_json::{Value, json};

/// A simulator's record as Shoal wrote it before the migration, owned by
/// `current` and last by `last`.
fn old_record(id: &str, state: &str, current: Value, last: Value) -> Value {
    json!({
        "id": id, "udid": "U", "device": "phone", "runtime": "ios",
        "workspace_id": current, "last_workspace_id": last,
        "lease_name": "default", "reason": null, "state": state,
        "last_used": 42, "error": null, "installed_apps": 2, "acquirer": {"kind": "user"}
    })
}

/// A database at the floor that holds `records`, each under its ID.
fn holding(records: &[(&str, Value)]) -> Result<Connection> {
    let db = at_floor()?;
    for (id, record) in records {
        db.execute(
            "INSERT INTO simulators(id,record) VALUES (?1,?2)",
            rusqlite::params![id, record.to_string()],
        )?;
    }
    Ok(db)
}

#[test]
fn state_and_owners_move_into_columns() -> Result<()> {
    let mut leased = old_record("leased", "leased", json!("A"), json!("B"));
    let idle = old_record("idle", "idle", Value::Null, json!("A"));
    // An earlier Shoal left out an owner it had none of.
    let mut without_owner = old_record("bare", "creating", Value::Null, Value::Null);
    without_owner
        .as_object_mut()
        .unwrap()
        .remove("workspace_id");
    let mut db = holding(&[
        ("leased", leased.clone()),
        ("idle", idle),
        ("bare", without_owner),
    ])?;
    migrate(&mut db)?;

    let mut statement = db.prepare(
        "SELECT id,state,workspace_id,last_workspace_id,record FROM simulators ORDER BY id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let columns: Vec<_> = (rows.iter())
        .map(|(id, state, current, last, _)| (&**id, &**state, current.as_deref(), last.as_deref()))
        .collect();
    assert_eq!(
        columns,
        [
            ("bare", "creating", None, None),
            ("idle", "idle", None, Some("A")),
            ("leased", "leased", Some("A"), Some("B")),
        ]
    );
    let record: Value = serde_json::from_str(&rows[2].4)?;
    let fields = leased.as_object_mut().unwrap();
    for moved in ["id", "state", "workspace_id", "last_workspace_id"] {
        fields.remove(moved);
    }
    assert_eq!(record, leased);
    Ok(())
}

/// The error migrating a database that holds `record` under `id` fails
/// with.
fn refusal(id: &str, record: Value) -> Result<String> {
    let mut db = holding(&[(id, record)])?;
    let error = migrate(&mut db).unwrap_err();
    let versions: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(versions, FLOOR, "a refused migration changes nothing");
    Ok(format!("{error:#}"))
}

#[test]
fn a_record_without_a_state_is_refused_by_its_row() -> Result<()> {
    let mut record = old_record("sim", "idle", Value::Null, Value::Null);
    record.as_object_mut().unwrap().remove("state");
    let error = refusal("sim", record)?;
    assert!(error.contains("simulators row sim"), "{error}");
    assert!(error.contains("no state"), "{error}");
    Ok(())
}

#[test]
fn a_record_naming_another_id_is_refused_by_its_row() -> Result<()> {
    let error = refusal("sim", old_record("other", "idle", Value::Null, Value::Null))?;
    assert!(error.contains("simulators row sim"), "{error}");
    assert!(error.contains("another"), "{error}");
    Ok(())
}
