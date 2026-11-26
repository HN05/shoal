//! Migration 305: the journal's columns, filled from the records before it.
use super::*;
use serde_json::{Value, json};

/// The columns of each journal row after migrating `records` from the
/// floor, with the sequence's next ID.
fn migrated_columns(records: &[Value]) -> Result<Vec<String>> {
    let mut db = at_floor()?;
    for record in records {
        db.execute(
            "INSERT INTO space_events(space_id,record) VALUES (?1,?2)",
            rusqlite::params![record["space_id"].as_str(), record.to_string()],
        )?;
    }
    // Retention deleted the newest event, whose ID stays taken.
    db.execute(
        "DELETE FROM space_events WHERE id=(SELECT MAX(id) FROM space_events)",
        [],
    )?;
    migrate(&mut db)?;
    let mut statement = db.prepare(
        "SELECT id,space_id,kind,bulk,repository_id,child_id FROM space_events ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(format!(
            "{} {:?} {} {} {:?} {:?}",
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut columns = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    db.execute(
        "INSERT INTO space_events(kind,bulk,record) VALUES ('daemon_started',0,'{}')",
        [],
    )?;
    columns.push(format!("next {}", db.last_insert_rowid()));
    Ok(columns)
}

#[test]
fn the_journal_s_columns_take_what_its_records_held() -> Result<()> {
    let space = |kind: &str, extra: Value| {
        let mut record = json!({"kind": kind, "space_id": "work", "repository_id": "repo"});
        record
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        record
    };
    let activity = |kind: &str| json!({"item_activity": {"kind": "pr", "url": "u", "update": {"kind": kind, "message": "m"}}});
    let columns = migrated_columns(&[
        space(
            "child_removed",
            json!({"child": {"id": "child", "name": "c", "kind": "chat"}}),
        ),
        space("agent_state", json!({"agent_state": {"state": "idle"}})),
        space("item_activity", activity("comment")),
        space("item_activity", activity("merged")),
        json!({"kind": "file_added", "space_id": "work", "repository_id": null}),
        json!({"kind": "daemon_started", "space_id": null, "version": "1"}),
        space("renamed", json!({})),
    ])?;
    assert_eq!(
        columns,
        [
            r#"1 Some("work") child_removed false Some("repo") Some("child")"#,
            r#"2 Some("work") agent_state true Some("repo") None"#,
            r#"3 Some("work") item_activity true Some("repo") None"#,
            r#"4 Some("work") item_activity false Some("repo") None"#,
            r#"5 Some("work") file_added false None None"#,
            "6 None daemon_started false None None",
            "next 8",
        ]
    );
    Ok(())
}
