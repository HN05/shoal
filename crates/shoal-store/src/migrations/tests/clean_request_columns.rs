//! Migration 327: a clean request's status moves into a column.
use super::*;
use serde_json::{Value, json};

/// A clean request's record as Shoal wrote it before the migration.
fn old_record(workspace: &str, request_id: &str, status: &str) -> Value {
    json!({
        "workspace_id": workspace, "workspace_name": "fix", "repository_id": "repo",
        "execution_id": null, "requested_at": 1, "updated_at": 2, "attempts": 1,
        "request": {"request_id": request_id, "clean": true, "name": "default",
            "profile": null, "device": null, "runtime": null, "reason": "first launch",
            "queue": false},
        "status": status, "action": "erase", "simulator_id": "sim", "udid": "U",
        "apps_removed": 2, "erase_completed": true, "evicted": [], "error": null
    })
}

/// A database at the floor that holds `rows`, each a request ID, its
/// workspace and its record.
fn holding(rows: &[(&str, &str, Value)]) -> Result<Connection> {
    let db = at_floor()?;
    for (request_id, workspace, record) in rows {
        db.execute(
            "INSERT INTO simulator_clean_requests(request_id,workspace_id,record)
                VALUES (?1,?2,?3)",
            rusqlite::params![request_id, workspace, record.to_string()],
        )?;
    }
    Ok(db)
}

#[test]
fn the_status_moves_into_its_column() -> Result<()> {
    let mut acquired = old_record("A", "r1", "acquired");
    let mut db = holding(&[
        ("r1", "A", acquired.clone()),
        ("r2", "B", old_record("B", "r2", "requested")),
    ])?;
    // History entry IDs continue past one an earlier Shoal deleted.
    db.execute(
        "DELETE FROM simulator_clean_requests WHERE request_id='r2'",
        [],
    )?;
    migrate(&mut db)?;

    let (id, request_id, workspace, status, record): (i64, String, String, String, String) = db
        .query_row(
            "SELECT id,request_id,workspace_id,status,record FROM simulator_clean_requests",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
    assert_eq!(
        (id, &*request_id, &*workspace, &*status),
        (1, "r1", "A", "acquired")
    );
    let fields = acquired.as_object_mut().unwrap();
    fields.remove("workspace_id");
    fields.remove("status");
    fields["request"]
        .as_object_mut()
        .unwrap()
        .remove("request_id");
    assert_eq!(serde_json::from_str::<Value>(&record)?, acquired);

    db.execute(
        "INSERT INTO simulator_clean_requests(request_id,workspace_id,status,record)
            VALUES ('r3','A','requested','{}')",
        [],
    )?;
    let next: i64 = db.query_row(
        "SELECT id FROM simulator_clean_requests WHERE request_id='r3'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(next, 3);
    Ok(())
}

/// The error migrating a database that holds the clean request
/// `request_id` of `workspace` with `record` fails with.
fn refusal(request_id: &str, workspace: &str, record: Value) -> Result<String> {
    let mut db = holding(&[(request_id, workspace, record)])?;
    let error = migrate(&mut db).unwrap_err();
    let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(version, FLOOR, "a refused migration changes nothing");
    Ok(format!("{error:#}"))
}

#[test]
fn a_record_without_a_status_is_refused_by_its_row() -> Result<()> {
    let mut record = old_record("A", "r1", "busy");
    record.as_object_mut().unwrap().remove("status");
    let error = refusal("r1", "A", record)?;
    assert!(error.contains("simulator_clean_requests row 1"), "{error}");
    assert!(error.contains("no status"), "{error}");
    Ok(())
}

#[test]
fn a_record_naming_another_workspace_is_refused_by_its_row() -> Result<()> {
    let error = refusal("r1", "A", old_record("B", "r1", "busy"))?;
    assert!(error.contains("simulator_clean_requests row 1"), "{error}");
    assert!(error.contains("another"), "{error}");
    Ok(())
}

#[test]
fn a_record_naming_another_request_id_is_refused_by_its_row() -> Result<()> {
    let error = refusal("r1", "A", old_record("A", "r9", "busy"))?;
    assert!(error.contains("simulator_clean_requests row 1"), "{error}");
    assert!(
        error.contains("request has no ID or another one"),
        "{error}"
    );
    Ok(())
}
