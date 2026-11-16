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
    migrate_to(&mut db, 204)?;
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

#[test]
fn fields_earlier_records_lacked_are_filled() -> Result<()> {
    let mut db = before(205)?;
    db.execute_batch(
        r#"INSERT INTO simulators(id,record) VALUES ('sim','{"id":"sim"}');
         INSERT INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
             VALUES ('w',NULL,0,'w',9,'{"agents":[{"record":{"agent":"claude"}}],
                 "last_sessions":[{"agent":"codex","mode":"plan"}]}');
         INSERT INTO space_files(space_id,space,source,name,path,bytes,created_at)
             VALUES ('w','w','added','a.md','/repo/a.md',1,1);
         INSERT INTO space_events(space_id,record) VALUES ('w',
             '{"kind":"linked","space_id":"w","link":{"kind":"pr","url":"https://forge/o/r/pulls/2"}}');"#,
    )?;
    migrate(&mut db)?;
    let simulator = record(&db, "simulators", "id='sim'")?;
    assert_eq!(simulator["acquirer"], json!({"kind": "user"}));
    let removed = record(&db, "removed_spaces", "space_id='w'")?;
    assert_eq!(removed["agents"][0]["record"]["mode"], "worker");
    let last = &removed["last_sessions"][0];
    assert_eq!(last["mode"], "plan");
    assert!(last["execution"].is_string(), "{last}");
    let attacher: String =
        db.query_row("SELECT attached_by FROM space_files", [], |row| row.get(0))?;
    assert_eq!(attacher, r#"{"kind":"user"}"#);
    let added = record(
        &db,
        "space_events",
        "json_extract(record,'$.kind')='file_added'",
    )?;
    assert_eq!(added["file"]["attached_by"], json!({"kind": "user"}));
    let linked = record(
        &db,
        "space_events",
        "json_extract(record,'$.kind')='linked'",
    )?;
    assert_eq!(linked["link"]["relation"], "assigned");
    Ok(())
}

#[test]
fn earlier_links_registrations_and_snapshots_take_the_current_shape() -> Result<()> {
    let mut db = before(206)?;
    db.execute_batch(
        r#"INSERT INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
             VALUES ('w',NULL,0,'w',9,'{"links":{"issue":"https://forge/o/r/issues/1",
                 "issue_title":"Fix it","issues":[{"kind":"issue",
                 "url":"https://forge/o/r/issues/2"}],"prs":["https://forge/o/r/pulls/3"]}}');
         INSERT INTO pr_cleanup(space_id,record)
             VALUES ('w','{"url":"https://forge/o/r/pulls/3","head":null,"error":null}');
         INSERT INTO pr_activity(space_id,url,record) VALUES ('w','https://forge/o/r/pulls/3',
             '{"comments":{"discussion:1":"hi",
                 "reviews:verdicts":"bob:Approved\nalice:ChangesRequested\nbob:Commented\n"}}');"#,
    )?;
    migrate(&mut db)?;
    let removed = record(&db, "removed_spaces", "space_id='w'")?;
    assert_eq!(
        removed["links"],
        json!({
            "issues": [
                {"kind": "issue", "url": "https://forge/o/r/issues/1", "title": "Fix it"},
                {"kind": "issue", "url": "https://forge/o/r/issues/2"},
            ],
            "prs": [{"kind": "pr", "url": "https://forge/o/r/pulls/3"}],
            "opened": [],
            "related": [],
        })
    );
    let links: shoal_protocol::model::SpaceLinks =
        serde_json::from_value(removed["links"].clone())?;
    assert_eq!(links.issue_title(), Some("Fix it"));
    let registration = record(&db, "pr_cleanup", "space_id='w'")?;
    assert_eq!(
        registration,
        json!({"urls": ["https://forge/o/r/pulls/3"], "head": null, "error": null})
    );
    let watch = record(&db, "pr_activity", "space_id='w'")?;
    assert_eq!(
        watch["comments"],
        json!({
            "discussion:1": {"fingerprint": "hi"},
            "reviews:verdict:alice": {"fingerprint": "ChangesRequested", "author": "alice"},
            "reviews:verdict:bob": {"fingerprint": "Approved,Commented", "author": "bob"},
        })
    );
    Ok(())
}
