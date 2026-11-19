use super::*;
use crate::migrations::{migrate, migrate_to};

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

#[test]
fn events_earlier_versions_journaled_take_the_current_shape() -> Result<()> {
    let mut db = before(207)?;
    db.execute_batch(
        r#"INSERT INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
             VALUES ('c',NULL,1,'planner',9,'{}');
         INSERT INTO space_events(space_id,record) VALUES
             ('w','{"kind":"continued","space_id":"w"}'),
             ('w','{"kind":"chat_changed","space_id":"w","chat":"c"}'),
             ('w','{"kind":"chat_changed","space_id":"w","chat":null}'),
             ('w','{"kind":"created","space_id":"w",
                 "parent_workspace":{"id":"p","name":"lead","branch":"main"}}'),
             ('w','{"kind":"review_ready","space_id":"w",
                 "review":{"kind":null,"url":null,"head":"abc"}}');"#,
    )?;
    migrate(&mut db)?;
    let events: Vec<Value> = db
        .prepare("SELECT record FROM space_events ORDER BY id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|record| Ok(serde_json::from_str(&record?)?))
        .collect::<Result<_>>()?;
    let field = |event: &Value, key: &str| event[key].clone();
    assert_eq!(field(&events[0], "kind"), "undone");
    assert_eq!(
        (field(&events[1], "kind"), field(&events[1], "parent")),
        (
            "parent_changed".into(),
            json!({"id": "c", "name": "planner", "kind": "chat"})
        )
    );
    assert_eq!(field(&events[2], "parent"), Value::Null);
    assert_eq!(
        field(&events[3], "parent"),
        json!({"id": "p", "name": "lead", "branch": "main", "kind": "workspace"})
    );
    assert_eq!(
        field(&events[4], "review"),
        json!({"kind": null, "url": null})
    );
    Ok(())
}

#[test]
fn a_git_directory_recorded_by_device_and_inode_takes_its_identity_now() -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let directory = tempfile::tempdir()?;
    let metadata = std::fs::metadata(directory.path())?;
    let earlier = format!("{}:{}", metadata.dev(), metadata.ino());
    let mut db = before(208)?;
    db.execute_batch(&format!(
        "BEGIN;
        INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        INSERT INTO workspaces(id,path,branch,state,git_dir,git_dir_id,base_commit)
            VALUES ('w','/work','topic','ready','{}','{earlier}','abc');
        INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','w');
        COMMIT;",
        directory.path().display()
    ))?;
    // Read before a later migration drops the column.
    migrate_to(&mut db, 208)?;
    let identity: String = db.query_row(
        "SELECT git_dir_id FROM workspaces WHERE id='w'",
        [],
        |row| row.get(0),
    )?;
    let now = super::super::frozen::directory_identity(directory.path())?;
    assert_eq!(identity, now);
    Ok(())
}

#[test]
fn a_snapshot_that_saw_no_state_leaves_it_out() -> Result<()> {
    let mut db = before(210)?;
    db.execute_batch(
        r#"INSERT INTO pr_activity(space_id,url,record)
             VALUES ('w','https://forge/o/r/pulls/1','{"state":"","comments":{}}');"#,
    )?;
    migrate(&mut db)?;
    let watch = record(&db, "pr_activity", "space_id='w'")?;
    assert_eq!(watch, json!({"comments": {}}));
    Ok(())
}

#[test]
fn saved_agent_files_take_the_current_shape() -> Result<()> {
    let state = tempfile::tempdir()?;
    let mut db = crate::test_support::open_database(state.path().join("state.db"))?;
    migrate_to(&mut db, 210)?;
    let space = state.path().join("workspaces/w");
    fs::create_dir_all(&space)?;
    let write = |name: &str, saved: Value| fs::write(space.join(name), saved.to_string());
    write(
        "e1.recovery.json",
        json!({"agent": "claude", "place": "w1:p2"}),
    )?;
    write(
        "e2.last-session.json",
        json!({"agent": "codex", "mode": "plan"}),
    )?;
    write(
        "last-session.json",
        json!({"agent": "claude", "session": "s1"}),
    )?;
    // A truncated record stays as it is without stopping the migration.
    fs::write(space.join("e3.recovery.json"), r#"{"agent":"#)?;
    migrate(&mut db)?;
    assert_eq!(
        fs::read_to_string(space.join("e3.recovery.json"))?,
        r#"{"agent":"#
    );
    let read = |path: &Path| -> Result<Value> { Ok(serde_json::from_slice(&fs::read(path)?)?) };
    assert_eq!(
        read(&space.join("e1.recovery.json"))?,
        json!({"agent": "claude", "mode": "worker", "origin": {"host": "herdr", "place": "w1:p2"}})
    );
    assert_eq!(
        read(&space.join("e2.last-session.json"))?,
        json!({"agent": "codex", "mode": "plan", "execution": "e2"})
    );
    assert!(!space.join("last-session.json").exists());
    let named: Vec<_> = (fs::read_dir(&space)?)
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()?;
    let [unnamed] = named
        .iter()
        .filter(|path| {
            ![
                "e1.recovery.json",
                "e2.last-session.json",
                "e3.recovery.json",
            ]
            .iter()
            .any(|name| path.ends_with(name))
        })
        .collect::<Vec<_>>()[..]
    else {
        panic!("one renamed last session: {named:?}");
    };
    let last = read(unnamed)?;
    let execution = last["execution"].as_str().unwrap_or_default();
    assert!(
        unnamed.ends_with(format!("{execution}.last-session.json")),
        "{last}"
    );
    assert_eq!(
        (last["session"].as_str(), last["mode"].as_str()),
        (Some("s1"), Some("worker"))
    );
    Ok(())
}

#[test]
fn files_a_chat_attached_become_the_users() -> Result<()> {
    let mut db = before(213)?;
    let agent = json!({"kind": "agent", "agent": "claude", "execution": "e1"});
    db.execute_batch(&format!(
        r#"INSERT INTO space_files(space_id,space,source,name,path,bytes,created_at,attached_by)
             VALUES ('w','w','added','a.md','/repo/a.md',1,1,
                 '{{"kind":"chat","agent":"claude","chat":"c"}}'),
             ('w','w','added','b.md','/repo/b.md',1,1,'{agent}');"#
    ))?;
    migrate(&mut db)?;
    let attacher = |name: &str| -> Result<Value> {
        let text: String = db.query_row(
            "SELECT attached_by FROM space_files WHERE name=?1",
            [name],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&text)?)
    };
    assert_eq!(attacher("a.md")?, json!({"kind": "user"}));
    assert_eq!(attacher("b.md")?, agent);
    let added = |name: &str| {
        record(
            &db,
            "space_events",
            &format!("json_extract(record,'$.file.name')='{name}'"),
        )
    };
    assert_eq!(
        added("a.md")?["file"]["attached_by"],
        json!({"kind": "user"})
    );
    assert_eq!(added("b.md")?["file"]["attached_by"], agent);
    Ok(())
}
