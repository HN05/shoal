//! Lifecycle events must share the mutation's transaction and survive removal.
use super::*;

#[test]
fn lifecycle_events_commit_and_roll_back_with_state() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    let tx = db.transaction()?;
    tx.execute_batch(
        "INSERT INTO workspaces(id,path,branch,state,observed_branch,base_commit,git_dir)
            VALUES ('w','/work','worker','preparing','worker','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','worker');",
    )?;
    tx.execute("UPDATE workspaces SET state='ready' WHERE id='w'", [])?;
    tx.rollback()?;
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM space_events", [], |r| r
            .get::<_, i64>(0))?,
        0
    );

    db.execute_batch(
        "BEGIN;
         INSERT INTO workspaces(id,path,branch,state,observed_branch,base_commit,git_dir)
            VALUES ('w','/work','worker','preparing','worker','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','worker');
         COMMIT;",
    )?;
    db.execute(
        "UPDATE workspaces SET state='failed',error='setup error' WHERE id='w'",
        [],
    )?;
    db.execute(
        "UPDATE workspaces SET state='ready',error=NULL WHERE id='w'",
        [],
    )?;
    db.execute("INSERT INTO space_completion(space_id,record,cause) VALUES ('w','{\"error\":null}','issue')", [])?;
    db.execute(
        "UPDATE space_completion SET record='{\"error\":\"retained\"}'",
        [],
    )?;
    db.execute("DELETE FROM space_completion WHERE space_id='w'", [])?;
    db.execute(
        "UPDATE workspaces SET observed_branch=NULL WHERE id='w'",
        [],
    )?;
    db.execute("DELETE FROM spaces WHERE id='w'", [])?;
    let records = db
        .prepare("SELECT record FROM space_events ORDER BY id")?
        .query_map([], |r| r.get::<_, String>(0))?
        .map(|r| Ok(serde_json::from_str::<serde_json::Value>(&r?)?))
        .collect::<Result<Vec<_>>>()?;
    let kinds: Vec<_> = records
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "created",
            "setup_failed",
            "ready",
            "completed",
            "undone",
            "branch_changed"
        ]
    );
    assert_eq!(records[1]["error"], "setup error");
    assert_eq!(records[3]["cause"], "issue");
    assert!(records[5]["branch"].is_null());
    assert!(records.iter().all(|r| r["space_id"] == "w"
        && r["space_kind"] == "workspace"
        && r["repository_id"] == "repo"));
    Ok(())
}

#[test]
fn each_space_keeps_its_newest_bulk_events_and_every_other() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    let insert = |db: &Connection, space: &str, record: &str| {
        db.execute(
            "INSERT INTO space_events(space_id,record) VALUES (?1,json_set(json(?2),'$.space_id',?1))",
            [space, record],
        )
    };
    let tx = db.transaction()?;
    insert(&tx, "busy", r#"{"kind":"created"}"#)?;
    insert(&tx, "quiet", r#"{"kind":"created"}"#)?;
    insert(&tx, "quiet", r#"{"kind":"agent_state"}"#)?;
    let merged = r#"{"kind":"item_activity","item_activity":{"update":{"kind":"merged"}}}"#;
    let comment = r#"{"kind":"item_activity","item_activity":{"update":{"kind":"comment"}}}"#;
    insert(&tx, "busy", merged)?;
    for turn in 0..600 {
        let record = if turn % 2 == 0 {
            r#"{"kind":"agent_state"}"#
        } else {
            comment
        };
        insert(&tx, "busy", record)?;
    }
    tx.commit()?;
    let count = |space: &str, bulk: bool| -> Result<i64> {
        Ok(db.query_row(
            "SELECT COUNT(*) FROM space_events WHERE space_id=?1 AND bulk=?2",
            rusqlite::params![space, bulk],
            |r| r.get(0),
        )?)
    };
    assert_eq!(count("busy", true)?, 500);
    assert_eq!(count("busy", false)?, 2);
    assert_eq!((count("quiet", true)?, count("quiet", false)?), (1, 1));
    let oldest_bulk: i64 = db.query_row(
        "SELECT MIN(id) FROM space_events WHERE space_id='busy' AND bulk",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(oldest_bulk, 105);
    db.execute("DELETE FROM space_events", [])?;
    insert(&db, "busy", r#"{"kind":"created"}"#)?;
    assert_eq!(db.last_insert_rowid(), 605);
    Ok(())
}

/// The journal's records, oldest first.
fn journal(db: &Connection) -> Result<Vec<shoal_protocol::events::EventDetails>> {
    db.prepare("SELECT record FROM space_events ORDER BY id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|record| Ok(serde_json::from_str(&record?)?))
        .collect()
}

#[test]
fn renames_threads_and_children_reach_their_spaces_logs() -> Result<()> {
    use shoal_protocol::events::EventKind;
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
             VALUES ('parent','/plan','plan','ready','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name)
             VALUES ('parent','workspace','repo','plan');
         INSERT INTO space_parents(space_id,parent_id) VALUES ('child','parent');
         INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
             VALUES ('child','/fix','fix','ready','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name)
             VALUES ('child','workspace','repo','fix');
         INSERT INTO space_parents(space_id,parent_id) VALUES ('chat','parent');
         INSERT INTO chats(id) VALUES ('chat');
         INSERT INTO spaces(id,kind,repository_id,name,mode)
             VALUES ('chat','chat','repo','helper','chat');
         UPDATE spaces SET name='Fix login' WHERE id='child';
         UPDATE workspaces SET branch='fix/login' WHERE id='child';
         INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener)
             VALUES (1,'child',2,1,'question','Which database?',1,'space');
         INSERT INTO thread_posts(thread_id,created_at,side,sender,sender_name,text)
             VALUES (1,1,'opener','space','fix','Postgres or SQLite?');
         INSERT INTO thread_posts(thread_id,created_at,side,sender,text,forwards,forwarded_to)
             VALUES (1,2,'holder','user','You decide',1,'parent');
         UPDATE threads SET outcome=json_object('outcome','answered'),resolved_by='space',resolved_at=3
             WHERE id=1;
         DELETE FROM spaces WHERE id='chat';
         DELETE FROM spaces WHERE id='child';
         COMMIT;",
    )?;
    let events: Vec<_> = journal(&db)?
        .into_iter()
        .filter(|event| !matches!(event.kind, EventKind::Created | EventKind::Ready))
        .map(|event| {
            let space = event.space_id.to_string();
            let detail = match event.kind {
                EventKind::ChildCreated | EventKind::ChildRemoved => {
                    event.child.map(|child| child.name.to_string())
                }
                EventKind::Renamed => Some(format!(
                    "{} {}",
                    event.name,
                    event
                        .space
                        .branch()
                        .map(ToString::to_string)
                        .unwrap_or_default()
                )),
                EventKind::ThreadOpened
                | EventKind::ThreadPosted
                | EventKind::ThreadForwarded
                | EventKind::ThreadResolved => event.thread.map(|thread| {
                    format!(
                        "{} {:?} {:?} {:?} {:?}",
                        thread.number, thread.sender, thread.text, thread.to, thread.outcome
                    )
                }),
                _ => None,
            };
            (space, event.kind, detail)
        })
        .collect();
    let some = |text: &str| Some(text.to_owned());
    assert_eq!(
        events,
        [
            ("parent".into(), EventKind::ChildCreated, some("fix")),
            ("parent".into(), EventKind::ChildCreated, some("helper")),
            ("child".into(), EventKind::Renamed, some("Fix login fix")),
            (
                "child".into(),
                EventKind::Renamed,
                some("Fix login fix/login")
            ),
            (
                "child".into(),
                EventKind::ThreadOpened,
                some("2.1 None None None None")
            ),
            (
                "child".into(),
                EventKind::ThreadPosted,
                some("2.1 Some(\"fix\") Some(\"Postgres or SQLite?\") None None")
            ),
            (
                "child".into(),
                EventKind::ThreadForwarded,
                some("2.1 Some(\"the user\") Some(\"You decide\") Some(\"plan\") None")
            ),
            (
                "child".into(),
                EventKind::ThreadResolved,
                some("2.1 None None None Some(Answered)")
            ),
            ("parent".into(), EventKind::ChildRemoved, some("helper")),
            ("parent".into(), EventKind::ChildRemoved, some("Fix login")),
        ]
    );
    Ok(())
}

#[test]
fn a_workspace_s_creation_takes_the_base_recorded_after_it() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         INSERT INTO workspaces(id,path,branch,state,pending_plan)
             VALUES ('w','/fix','fix','preparing','{}');
         INSERT INTO spaces(id,kind,repository_id,name)
             VALUES ('w','workspace','repo','fix');
         UPDATE workspaces SET base_commit='abc',base_ref='refs/heads/main' WHERE id='w';
         UPDATE workspaces SET base_ref='refs/heads/other' WHERE id='w';
         COMMIT;",
    )?;
    let created: Vec<_> = journal(&db)?
        .into_iter()
        .filter(|event| event.kind == shoal_protocol::events::EventKind::Created)
        .map(|event| event.base_ref)
        .collect();
    assert_eq!(created, [Some("refs/heads/main".to_owned())]);
    Ok(())
}
