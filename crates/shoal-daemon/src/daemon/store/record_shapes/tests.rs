use super::*;
use crate::daemon::store::migrations::{migrate, migrate_to};

/// A database at the schema before migration `version`.
fn before(version: i64) -> Result<Connection> {
    let mut db = Connection::open_in_memory()?;
    migrate_to(&mut db, version - 1)?;
    Ok(db)
}

/// The record of `table`'s row that `key` selects.
fn record(db: &Connection, table: &str, key: &str) -> Result<Value> {
    let text: String = db.query_row(
        &format!("SELECT record FROM {table} WHERE {key}"),
        [],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&text)?)
}

#[test]
fn renamed_fields_take_their_new_names() -> Result<()> {
    let mut db = before(204)?;
    db.execute_batch(
        r#"INSERT INTO pr_activity(space_id,url,record)
             VALUES ('w','https://forge/o/r/pulls/1','{"own_state_at":7}');
         INSERT INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
             VALUES ('w',NULL,0,'w',9,'{"agents":[{"record":{"agent":"claude","place":"w1:p2"}}],
                 "last_session":{"agent":"codex","place":{"host":"rex","place":"b"}}}');"#,
    )?;
    migrate(&mut db)?;
    let watch = record(&db, "pr_activity", "space_id='w'")?;
    assert_eq!(watch, json!({"own_change_at": 7}));
    let removed = record(&db, "removed_spaces", "space_id='w'")?;
    assert_eq!(
        removed,
        json!({
            "agents": [{"record": {"agent": "claude", "origin": {"host": "herdr", "place": "w1:p2"}}}],
            "last_sessions": [{"agent": "codex", "origin": {"host": "rex", "place": "b"}}],
        })
    );
    Ok(())
}
