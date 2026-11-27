//! Migration 322: each resolved thread's outcome split into its columns.
use super::*;
use serde_json::{Value, json};

/// A database at the floor with a workspace whose threads, one per entry
/// of `outcomes`, are resolved with it, or open without one.
fn at_floor_with(outcomes: &[Option<Value>]) -> Result<Connection> {
    let db = at_floor()?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        BEGIN;
        INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
            VALUES ('work','/work','worker','ready','abc','/admin');
        INSERT INTO spaces(id,kind,repository_id,name,number)
            VALUES ('work','workspace','repo','worker',3);
        COMMIT;",
    )?;
    for (id, outcome) in (1..).zip(outcomes) {
        db.execute(
            "INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener,
                    outcome,resolved_by,resolved_at)
                VALUES (?1,'work',3,?1,'review','Finding',1,'shoal',?2,?3,?4)",
            rusqlite::params![
                id,
                outcome.as_ref().map(Value::to_string),
                outcome.as_ref().map(|_| "space"),
                outcome.as_ref().map(|_| 2),
            ],
        )?;
    }
    Ok(db)
}

#[test]
fn each_outcome_keeps_its_kind_and_cause_in_columns() -> Result<()> {
    let mut db = at_floor_with(&[
        None,
        Some(json!({"outcome": "answered"})),
        Some(json!({"outcome": "closed", "cause": "removal"})),
        Some(json!({"outcome": "withdrawn", "reason": "merged"})),
        Some(json!({"outcome": "duplicate",
            "of": {"space_number": 3, "item": 1, "number": "3.1"}})),
    ])?;
    migrate(&mut db)?;
    let mut reading =
        db.prepare("SELECT outcome,cause,outcome_details FROM threads ORDER BY id")?;
    let rows = reading
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let rows: Vec<_> = (rows.into_iter())
        .map(|(kind, cause, details)| {
            let details = details.map(|text| serde_json::from_str::<Value>(&text).unwrap());
            (kind, cause, details)
        })
        .collect();
    let text = |text: &str| Some(text.to_owned());
    assert_eq!(
        rows,
        [
            (None, None, None),
            (text("answered"), None, None),
            (text("closed"), text("removal"), None),
            (text("withdrawn"), None, Some(json!({"reason": "merged"}))),
            (
                text("duplicate"),
                None,
                Some(json!({"of": {"space_number": 3, "item": 1, "number": "3.1"}}))
            ),
        ]
    );
    // Splitting an outcome journals no resolution.
    let resolved: i64 = db.query_row(
        "SELECT COUNT(*) FROM space_events WHERE kind='thread_resolved'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(resolved, 0);
    Ok(())
}

#[test]
fn a_closed_outcome_without_its_cause_fails_the_migration() -> Result<()> {
    let mut db = at_floor_with(&[Some(json!({"outcome": "closed"}))])?;
    let error = format!("{:#}", migrate(&mut db).unwrap_err());
    assert!(
        error.contains("threads row 1") && error.contains("no cause"),
        "{error}"
    );
    Ok(())
}
