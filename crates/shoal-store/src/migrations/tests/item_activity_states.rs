//! Migrations 311 and 312: what a space last saw of an item keeps the
//! item's state in a column.
use super::*;
use rusqlite::params;
use serde_json::{Value, json};

/// The tables that hold what a space last saw of an item.
const TABLES: &[&str] = &["pr_activity", "space_item_activity"];

/// A database at the floor whose `table` holds `records`, the item of each
/// numbered from 1.
fn with_records(table: &str, records: &[Value]) -> Result<Connection> {
    let db = at_floor()?;
    for (item, record) in (1..).zip(records) {
        db.execute(
            &format!("INSERT INTO {table}(space_id,url,record) VALUES ('space',?1,?2)"),
            params![format!("item{item}"), record.to_string()],
        )?;
    }
    Ok(db)
}

#[test]
fn each_record_s_state_moves_into_its_column() -> Result<()> {
    for table in TABLES {
        let records = [
            json!({"kind": "pr", "comments": {}, "checks": {}, "state": "merged"}),
            json!({"kind": "run", "comments": {}, "checks": {}, "state": "failed"}),
            // A branch's merge check, and a watch before its first read.
            json!({"kind": "branch", "comments": {}, "checks": {}}),
            json!({"comments": {}, "checks": {}}),
        ];
        let mut db = with_records(table, &records)?;
        migrate(&mut db)?;
        let mut reading = db.prepare(&format!("SELECT state,record FROM {table} ORDER BY url"))?;
        let rows = reading
            .query_map([], |row| {
                Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let rows: Vec<(Option<String>, Value)> = (rows.into_iter())
            .map(|(state, record)| Ok((state, serde_json::from_str(&record)?)))
            .collect::<Result<_>>()?;
        assert_eq!(
            rows,
            [
                (
                    Some("merged".into()),
                    json!({"kind": "pr", "comments": {}, "checks": {}})
                ),
                (
                    Some("failed".into()),
                    json!({"kind": "run", "comments": {}, "checks": {}})
                ),
                (
                    None,
                    json!({"kind": "branch", "comments": {}, "checks": {}})
                ),
                (None, json!({"comments": {}, "checks": {}})),
            ],
            "{table}"
        );
    }
    Ok(())
}

/// The error migrating `record` in `table` fails with.
fn refusal(table: &str, record: Value) -> Result<String> {
    let mut db = with_records(table, &[json!({"comments": {}}), record])?;
    let error = migrate(&mut db).expect_err("the migration takes the record");
    Ok(format!("{error:#}"))
}

#[test]
fn a_record_in_a_state_no_item_takes_fails_the_migration_naming_its_row() -> Result<()> {
    for table in TABLES {
        let error = refusal(table, json!({"comments": {}, "state": "reopened"}))?;
        assert!(
            error.contains(&format!("{table} row of space space and item item2")),
            "{error}"
        );
    }
    Ok(())
}

#[test]
fn a_record_that_is_no_json_object_fails_the_migration() -> Result<()> {
    let error = refusal("space_item_activity", json!(["open"]))?;
    assert!(error.contains("item item2"), "{error}");
    Ok(())
}
