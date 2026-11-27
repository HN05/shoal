//! Migrations 313 and 314: a conversion's target and preparation move out
//! of its record into columns.
use super::*;
use serde_json::{Value, json};

/// Each conversion's space, target, preparation and remaining record after
/// migrating `records` from the floor.
type Migrated = (String, String, Option<String>, Value);

fn migrated(records: &[(&str, Value)]) -> Result<Vec<Migrated>> {
    let mut db = at_floor()?;
    insert(&db, records)?;
    migrate(&mut db)?;
    let mut statement = db.prepare(
        "SELECT space_id,target,preparation,record FROM space_conversions ORDER BY space_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (id, target, preparation, record) = row?;
        Ok((id, target, preparation, serde_json::from_str(&record)?))
    })
    .collect()
}

fn insert(db: &Connection, records: &[(&str, Value)]) -> Result<()> {
    for (id, record) in records {
        db.execute(
            "INSERT INTO space_conversions(space_id,record) VALUES (?1,?2)",
            rusqlite::params![id, record.to_string()],
        )?;
    }
    Ok(())
}

/// A chat's row as a conversion to a workspace keeps it.
fn chat_row() -> Value {
    json!({"scope_token": "token", "id": "c", "workspace_id": null, "root_dir": null,
        "repository_id": "repo", "name": "chat", "number": 3, "mode": "worker",
        "narrowing": null})
}

#[test]
fn each_conversion_s_target_and_preparation_move_into_columns() -> Result<()> {
    let prepared = json!({"to": "workspace", "mode": "worker", "chat": chat_row(),
        "execution": "e1", "preparation": "op1", "prepared": true, "hook_failure": null});
    let unprepared = json!({"to": "workspace", "mode": "worker", "chat": chat_row(),
        "execution": "e2"});
    let chat = json!({"to": "chat", "held": true, "chat": {"agent": "claude",
        "execution": "e3", "origin": null, "mode": "chat", "narrowing": {}}});
    let agent = json!({"to": "agent", "execution": "e4", "taking": {"agent": "codex"}});

    let rows = migrated(&[
        ("a", prepared),
        ("b", unprepared),
        ("c", chat),
        ("d", agent),
    ])?;

    let row = |id: &str, target: &str, preparation: Option<&str>, record: Value| {
        (
            id.to_owned(),
            target.to_owned(),
            preparation.map(str::to_owned),
            record,
        )
    };
    assert_eq!(
        rows,
        [
            row(
                "a",
                "workspace",
                Some("op1"),
                json!({"mode": "worker", "chat": chat_row(), "execution": "e1",
                    "prepared": true, "hook_failure": null})
            ),
            row(
                "b",
                "workspace",
                None,
                json!({"mode": "worker", "chat": chat_row(), "execution": "e2"})
            ),
            row(
                "c",
                "chat",
                None,
                json!({"held": true, "chat": {"agent": "claude", "execution": "e3",
                    "origin": null, "mode": "chat", "narrowing": {}}})
            ),
            row(
                "d",
                "agent",
                None,
                json!({"execution": "e4", "taking": {"agent": "codex"}})
            ),
        ]
    );
    Ok(())
}

/// The error migrating `record` as space `bad`'s conversion fails with.
fn refusal(record: &str) -> String {
    let mut db = at_floor().unwrap();
    db.execute(
        "INSERT INTO space_conversions(space_id,record) VALUES ('bad',?1)",
        [record],
    )
    .unwrap();
    let error = migrate(&mut db).expect_err("the migration refuses the row");
    format!("{error:#}")
}

#[test]
fn a_record_without_a_known_target_fails_the_migration_naming_its_row() {
    let error = refusal(r#"{"to": "elsewhere"}"#);
    assert!(error.contains("space_conversions row bad"), "{error}");
    assert!(error.contains("elsewhere"), "{error}");
}

#[test]
fn a_record_that_is_not_json_fails_the_migration_naming_its_row() {
    let error = refusal("not json");
    assert!(error.contains("space_conversions row bad"), "{error}");
}
