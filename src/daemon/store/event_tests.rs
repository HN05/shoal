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
    tx.execute("INSERT INTO workspaces(id,repository_id,name,path,branch,state,observed_branch) VALUES ('w','repo','worker','/work','worker','preparing','worker')", [])?;
    tx.execute("UPDATE workspaces SET state='ready' WHERE id='w'", [])?;
    tx.rollback()?;
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM workspace_events", [], |r| r
            .get::<_, i64>(0))?,
        0
    );

    db.execute("INSERT INTO workspaces(id,repository_id,name,path,branch,state,observed_branch) VALUES ('w','repo','worker','/work','worker','preparing','worker')", [])?;
    db.execute(
        "UPDATE workspaces SET state='failed',error='setup error' WHERE id='w'",
        [],
    )?;
    db.execute(
        "UPDATE workspaces SET state='ready',error=NULL WHERE id='w'",
        [],
    )?;
    db.execute("INSERT INTO workspace_completion(workspace_id,record,cause) VALUES ('w','{\"error\":null}','issue')", [])?;
    db.execute(
        "UPDATE workspace_completion SET record='{\"error\":\"retained\"}'",
        [],
    )?;
    db.execute(
        "DELETE FROM workspace_completion WHERE workspace_id='w'",
        [],
    )?;
    db.execute(
        "UPDATE workspaces SET observed_branch=NULL WHERE id='w'",
        [],
    )?;
    db.execute("DELETE FROM workspaces WHERE id='w'", [])?;
    let records = db
        .prepare("SELECT record FROM workspace_events ORDER BY id")?
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
    assert!(
        records
            .iter()
            .all(|r| r["workspace_id"] == "w" && r["repository_id"] == "repo")
    );
    Ok(())
}

#[test]
fn link_events_include_item_kind_and_url() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    db.execute(
        "INSERT INTO workspaces(id,repository_id,name,path,branch,state,observed_branch)
         VALUES ('w','repo','worker','/work','worker','ready','worker')",
        [],
    )?;
    crate::daemon::events::record_link(
        &db,
        "w",
        crate::daemon::events::EventKind::Linked,
        crate::model::Relation::Assigned,
        crate::forge::items::link::ItemKind::Pr,
        "https://forge.example/team/repo/pulls/7",
    )?;
    let record: String = db.query_row(
        "SELECT record FROM workspace_events WHERE json_extract(record,'$.kind')='linked'",
        [],
        |row| row.get(0),
    )?;
    let record: serde_json::Value = serde_json::from_str(&record)?;
    assert_eq!(record["link"]["kind"], "pr");
    assert_eq!(
        record["link"]["url"],
        "https://forge.example/team/repo/pulls/7"
    );
    Ok(())
}

#[test]
fn item_activity_events_include_item_and_update_details() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    db.execute(
        "INSERT INTO workspaces(id,repository_id,name,path,branch,state,observed_branch)
         VALUES ('w','repo','worker','/work','worker','ready','worker')",
        [],
    )?;
    let update = crate::forge::updates::Update {
        url: "https://forge.example/team/repo/pulls/7".into(),
        kind: crate::forge::updates::UpdateKind::Comment,
        message: "read the new review".into(),
        delivery: String::new(),
        check: None,
        workspace: None,
    };
    crate::daemon::events::record_item_activity(
        &db,
        "w",
        crate::forge::items::link::LinkKind::Pr,
        &update.url,
        &update,
    )?;
    let record: String = db.query_row(
        "SELECT record FROM workspace_events WHERE json_extract(record,'$.kind')='item_activity'",
        [],
        |row| row.get(0),
    )?;
    let record: serde_json::Value = serde_json::from_str(&record)?;
    assert_eq!(record["item_activity"]["kind"], "pr");
    assert_eq!(record["item_activity"]["url"], update.url);
    assert_eq!(record["item_activity"]["update"]["kind"], "comment");
    assert_eq!(record["item_activity"]["update"]["message"], update.message);
    Ok(())
}

#[test]
fn each_space_keeps_its_newest_bulk_events_and_every_other() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    let insert = |db: &Connection, space: &str, record: &str| {
        db.execute(
            "INSERT INTO workspace_events(record) VALUES (json_set(json(?2),'$.workspace_id',?1))",
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
            "SELECT COUNT(*) FROM workspace_events WHERE space_id=?1 AND bulk=?2",
            rusqlite::params![space, bulk],
            |r| r.get(0),
        )?)
    };
    assert_eq!(count("busy", true)?, 500);
    assert_eq!(count("busy", false)?, 2);
    assert_eq!((count("quiet", true)?, count("quiet", false)?), (1, 1));
    let oldest_bulk: i64 = db.query_row(
        "SELECT MIN(id) FROM workspace_events WHERE space_id='busy' AND bulk",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(oldest_bulk, 105);
    db.execute("DELETE FROM workspace_events", [])?;
    insert(&db, "busy", r#"{"kind":"created"}"#)?;
    assert_eq!(db.last_insert_rowid(), 605);
    Ok(())
}

/// Remove the current event schema when tests emulate an older database.
pub(super) fn remove_schema(db: &Connection) -> Result<()> {
    // The journal knows no spaces, and only workspaces record into it.
    db.execute_batch(
        "DROP TRIGGER chat_completed; DROP TRIGGER chat_undone;
         DROP TRIGGER space_review_ready; DROP TRIGGER space_review_ready_again;
         DROP TRIGGER space_review_cleared; DROP TRIGGER space_agent_state_set;
         DROP TRIGGER space_agent_state_changed; DROP TRIGGER space_agent_state_cleared;
         DROP TRIGGER workspace_renamed; DROP TRIGGER question_asked;
         DROP TRIGGER question_answered; DROP TRIGGER workspace_child_created;
         DROP TRIGGER chat_child_created; DROP TRIGGER workspace_base_recorded;
         DROP TRIGGER workspace_name_changed;
         DROP TRIGGER workspace_left_tree;
         DROP TRIGGER chat_left_tree;
         CREATE TRIGGER workspace_left_tree AFTER DELETE ON workspaces BEGIN
             DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
         END;
         CREATE TRIGGER chat_left_tree AFTER DELETE ON chats BEGIN
             DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
         END;
         DROP VIEW journal_spaces;
         DROP INDEX workspace_events_space; DROP TRIGGER workspace_events_retention;
         ALTER TABLE workspace_events DROP COLUMN bulk;
         ALTER TABLE workspace_events DROP COLUMN space_id;
         CREATE TRIGGER workspace_events_retention AFTER INSERT ON workspace_events BEGIN
             DELETE FROM workspace_events WHERE id <= (
                 SELECT id FROM workspace_events ORDER BY id DESC LIMIT 1 OFFSET 1000
             );
         END;",
    )?;
    // Workspaces record no copies again.
    db.execute_batch(
        "ALTER TABLE workspaces DROP COLUMN copied_from;
         ALTER TABLE workspaces DROP COLUMN copied_from_checkpoint;
         ALTER TABLE workspaces DROP COLUMN starting_checkpoint;
         ALTER TABLE workspaces DROP COLUMN starting_index;",
    )?;
    // Local merges go unrecorded again.
    db.execute_batch("DROP TABLE merges;")?;
    // Spaces have no numbers again; chats take their triggers with them.
    db.execute_batch(
        "DROP TRIGGER workspace_numbered; DROP TRIGGER chat_started;
         DROP VIEW free_space_number; DROP VIEW space_numbers;
         DROP INDEX workspaces_number; ALTER TABLE workspaces DROP COLUMN number;",
    )?;
    db.execute_batch("DROP TABLE changed_on_both_sides;")?;
    // Pending updates sit in their activity records again.
    db.execute_batch(
        "DROP TRIGGER workspace_pending_updates_removed;
         DROP TRIGGER chat_pending_updates_removed; DROP TABLE pending_updates;",
    )?;
    // Links name their workspace again, as before chats linked items.
    db.execute_batch(
        "DROP TRIGGER workspace_links_removed; DROP TRIGGER chat_links_removed;
         ALTER TABLE space_issue RENAME TO workspace_issue;
         ALTER TABLE workspace_issue RENAME COLUMN space_id TO workspace_id;
         ALTER TABLE pr_cleanup RENAME COLUMN space_id TO workspace_id;
         ALTER TABLE space_items RENAME TO workspace_items;
         ALTER TABLE workspace_items RENAME COLUMN space_id TO workspace_id;
         ALTER TABLE pr_activity RENAME COLUMN space_id TO workspace_id;
         ALTER TABLE space_item_activity RENAME TO workspace_item_activity;
         ALTER TABLE workspace_item_activity RENAME COLUMN space_id TO workspace_id;",
    )?;
    // Ready-for-review marks, agent states and chats record into the
    // journal, so they go with it.
    db.execute_batch(
        "DROP TRIGGER space_parent_set; DROP TRIGGER space_parent_changed;
         DROP TRIGGER space_parent_cleared; DROP TRIGGER workspace_left_tree;
         DROP TRIGGER chat_left_tree; DROP TABLE space_parents;
         DROP TRIGGER workspace_grace_removed; DROP TRIGGER chat_grace_removed;
         DROP TABLE chats;",
    )?;
    db.execute_batch(
        "DROP TRIGGER workspace_review_removed; DROP TABLE space_review;
         DROP TRIGGER workspace_agent_state_removed; DROP TABLE space_agent_state;",
    )?;
    for trigger in [
        "workspace_events_retention",
        "workspace_created",
        "workspace_ready",
        "workspace_setup_failed",
        "workspace_branch_changed",
        "workspace_completed",
        "workspace_completed_again",
        "workspace_undone",
        "workspace_base_removed",
        "demo_started",
        "demo_stopped",
        "simulator_acquired",
        "simulator_released",
        "simulator_deleted",
        "file_added",
        "file_removed",
    ] {
        db.execute_batch(&format!("DROP TRIGGER {trigger};"))?;
    }
    db.execute_batch("DROP TABLE workspace_events; ALTER TABLE workspaces DROP COLUMN observed_branch; ALTER TABLE workspace_completion DROP COLUMN cause;")?;
    // Later migrations add these columns and tables again when the emulated
    // database upgrades.
    db.execute_batch("ALTER TABLE workspace_holds DROP COLUMN from_continuation;")?;
    db.execute_batch("ALTER TABLE workspace_holds DROP COLUMN placed_by;")?;
    db.execute_batch("ALTER TABLE notifications DROP COLUMN workspace_id;")?;
    db.execute_batch("ALTER TABLE notifications DROP COLUMN subject;")?;
    db.execute_batch(
        "ALTER TABLE notifications DROP COLUMN files; ALTER TABLE questions DROP COLUMN files;",
    )?;
    db.execute_batch("ALTER TABLE chat_messages DROP COLUMN workspace_id;")?;
    db.execute_batch("ALTER TABLE executions DROP COLUMN scope_token;")?;
    db.execute_batch("ALTER TABLE executions DROP COLUMN agent_execution;")?;
    db.execute_batch("ALTER TABLE executions DROP COLUMN agent;")?;
    db.execute_batch("ALTER TABLE executions DROP COLUMN asks_in_chat;")?;
    db.execute_batch(
        "ALTER TABLE executions DROP COLUMN model; ALTER TABLE executions DROP COLUMN effort;",
    )?;
    db.execute_batch(
        "DROP INDEX workspaces_base; ALTER TABLE workspaces DROP COLUMN base_workspace_id;",
    )?;
    db.execute_batch("ALTER TABLE workspace_issue DROP COLUMN title;")?;
    db.execute_batch("DROP TABLE agent_queue;")?;
    db.execute_batch("ALTER TABLE repository_removals DROP COLUMN workspace_paths;")?;
    db.execute_batch("DROP TABLE repository_agent_environments;")?;
    db.execute_batch("DROP TABLE workspace_item_activity;")?;
    db.execute_batch("ALTER TABLE workspaces DROP COLUMN active_at;")?;
    db.execute_batch("ALTER TABLE workspaces DROP COLUMN title;")?;
    db.execute_batch("DROP TABLE removed_workspaces;")?;
    db.execute_batch(
        "DROP INDEX questions_number; ALTER TABLE questions DROP COLUMN number;
         ALTER TABLE questions DROP COLUMN closed_at;
         ALTER TABLE questions DROP COLUMN withdrawn;
         ALTER TABLE questions DROP COLUMN in_chat;
         DROP TABLE access_requests;
         CREATE TABLE access_requests (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            target_key TEXT NOT NULL, name TEXT NOT NULL,
            record TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1))
         );
         CREATE UNIQUE INDEX access_request_name
            ON access_requests(workspace_id,target_key,name) WHERE active=1;
         CREATE INDEX access_request_target ON access_requests(workspace_id,target_key);",
    )?;
    db.execute_batch("DROP TABLE workspace_items;")?;
    db.execute_batch("DROP TABLE checkpoints; DROP TABLE checkpoint_numbers;")?;
    db.execute_batch("DROP TABLE kept_branches;")?;
    db.execute_batch("DROP TABLE forge_hosts;")?;
    db.execute_batch("DROP TABLE demos;")?;
    db.execute_batch(
        "ALTER TABLE repositories DROP COLUMN contribution; DROP TABLE contribution_grace;",
    )?;
    db.execute_batch("DROP TABLE repository_moves;")?;
    db.execute_batch("DROP TABLE workspace_turn_retries; DROP TABLE execution_upstream_resets;")?;
    db.execute_batch("DROP TABLE execution_unretried_turns;")?;
    db.execute_batch("DROP TABLE session_cleanup; DROP TABLE owned_places;")?;
    db.execute_batch("ALTER TABLE executions DROP COLUMN origin;")?;
    db.execute_batch("ALTER TABLE chat_messages DROP COLUMN attachments;")?;
    db.execute_batch("ALTER TABLE agent_messages DROP COLUMN attachments;")?;
    db.execute_batch("ALTER TABLE agent_messages DROP COLUMN wakes;")?;
    db.execute_batch("DROP TABLE workspace_files; DROP TABLE ended_chats;")?;
    db.execute_batch("DROP TABLE earlier_attached_files;")?;
    db.execute_batch(
        "ALTER TABLE workspaces DROP COLUMN start_commit;
         ALTER TABLE workspaces DROP COLUMN made_for;
         ALTER TABLE workspaces DROP COLUMN setup_skipped;
         ALTER TABLE workspaces DROP COLUMN disposable;",
    )?;
    db.execute_batch(
        "ALTER TABLE workspaces DROP COLUMN mode; ALTER TABLE executions DROP COLUMN mode;",
    )?;
    Ok(())
}

#[test]
fn run_link_activity_and_approval_events_read_back() -> Result<()> {
    use crate::{daemon::events::EventDetails, forge::items::link::LinkKind};
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    db.execute(
        "INSERT INTO workspaces(id,repository_id,name,path,branch,state,observed_branch)
         VALUES ('w','repo','worker','/work','worker','ready','worker')",
        [],
    )?;
    let url = "https://forge.example/team/repo/actions/runs/2765";
    crate::daemon::events::record_link(
        &db,
        "w",
        crate::daemon::events::EventKind::Linked,
        crate::model::Relation::Related,
        LinkKind::Ci,
        url,
    )?;
    let update = crate::forge::updates::Update {
        url: url.into(),
        kind: crate::forge::updates::UpdateKind::CiCompleted,
        message: "Run finished".into(),
        delivery: String::new(),
        check: None,
        workspace: None,
    };
    crate::daemon::events::record_item_activity(&db, "w", LinkKind::Ci, url, &update)?;
    crate::daemon::events::record_item(&db, "w", LinkKind::Ci, url, "approve")?;
    let records = db
        .prepare("SELECT record FROM workspace_events ORDER BY id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let events = records
        .iter()
        .map(|record| serde_json::from_str::<EventDetails>(record))
        .collect::<serde_json::Result<Vec<_>>>()?;
    let kinds = events
        .iter()
        .filter_map(|event| {
            let link = event.link.as_ref().map(|link| link.kind);
            let item = event.item.as_ref().map(|item| item.kind);
            let activity = event.item_activity.as_ref().map(|activity| activity.kind);
            link.or(item).or(activity)
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, [LinkKind::Ci, LinkKind::Ci, LinkKind::Ci]);
    Ok(())
}

#[test]
fn chat_events_reach_the_journal_under_the_chat() -> Result<()> {
    use crate::daemon::events::{EventDetails, EventKind};
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         INSERT INTO chats(id,repository_id,name,agent,pid,birth,started_at)
             VALUES ('c','repo','planner','claude',1,'birth',1);
         INSERT INTO space_review(space_id,url,kind,head) VALUES ('c','',NULL,'abc');",
    )?;
    crate::daemon::events::record_link(
        &db,
        "c",
        EventKind::Linked,
        crate::model::Relation::Assigned,
        crate::forge::items::link::LinkKind::Issue,
        "https://forge.example/team/repo/issues/1",
    )?;
    db.execute_batch(
        "UPDATE chats SET completion='{\"head\":\"abc\",\"cleanup\":true,\"error\":null}' WHERE id='c';
         UPDATE chats SET completion=NULL WHERE id='c';",
    )?;
    let rows = db
        .prepare(
            "SELECT space_id,record FROM workspace_events
                WHERE json_extract(record,'$.kind') NOT LIKE 'chat_%' ORDER BY id",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let events = rows
        .iter()
        .map(|(space, record)| {
            Ok((
                space.as_str(),
                serde_json::from_str::<EventDetails>(record)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let kinds: Vec<_> = events.iter().map(|(_, event)| event.kind).collect();
    assert_eq!(
        kinds,
        [
            EventKind::ReviewReady,
            EventKind::Linked,
            EventKind::Completed,
            EventKind::Undone
        ]
    );
    for (space, event) in &events {
        assert_eq!(*space, "c");
        assert_eq!(event.space_id(), "c");
        assert_eq!(event.workspace_id, None);
        assert_eq!(event.name, "planner");
    }
    Ok(())
}

/// The journal's records, oldest first.
fn journal(db: &Connection) -> Result<Vec<crate::daemon::events::EventDetails>> {
    db.prepare(
        "SELECT record FROM workspace_events
            WHERE json_extract(record,'$.kind') NOT LIKE 'chat_%' ORDER BY id",
    )?
    .query_map([], |row| row.get::<_, String>(0))?
    .map(|record| Ok(serde_json::from_str(&record?)?))
    .collect()
}

#[test]
fn titles_questions_and_children_reach_their_spaces_logs() -> Result<()> {
    use crate::daemon::events::EventKind;
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         INSERT INTO workspaces(id,repository_id,name,path,branch,state)
             VALUES ('parent','repo','plan','/plan','plan','ready');
         INSERT INTO space_parents(space_id,parent_id) VALUES ('child','parent');
         INSERT INTO workspaces(id,repository_id,name,path,branch,state)
             VALUES ('child','repo','fix','/fix','fix','ready');
         INSERT INTO chats(id,repository_id,name,agent,pid,birth,started_at)
             VALUES ('chat','repo','helper','claude',1,'birth',1);
         INSERT INTO space_parents(space_id,parent_id) VALUES ('chat','parent');
         UPDATE workspaces SET title='Fix login' WHERE id='child';
         UPDATE workspaces SET name='fix-login',branch='fix/login' WHERE id='child';
         INSERT INTO questions(space_number,number,workspace_id,created_at,question,
                 recommendation,choices)
             VALUES (2,1,'child',1,'Which database?','SQLite','[]');
         UPDATE questions SET answer='SQLite',answered_at=2 WHERE number=1;
         DELETE FROM chats WHERE id='chat';
         DELETE FROM workspaces WHERE id='child';",
    )?;
    let events: Vec<_> = journal(&db)?
        .into_iter()
        .filter(|event| !matches!(event.kind, EventKind::Created | EventKind::Ready))
        .map(|event| {
            let space = event.space_id().to_owned();
            let detail = match event.kind {
                EventKind::ChildCreated | EventKind::ChildRemoved => {
                    event.child.map(|child| child.name.to_string())
                }
                EventKind::Renamed => event.title.flatten().or(Some(event.name)),
                EventKind::QuestionAsked | EventKind::QuestionAnswered => event
                    .question
                    .map(|question| format!("{} {:?}", question.number, question.answer)),
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
            ("child".into(), EventKind::Renamed, some("Fix login")),
            ("child".into(), EventKind::Renamed, some("fix-login")),
            ("child".into(), EventKind::QuestionAsked, some("2.1 None")),
            (
                "child".into(),
                EventKind::QuestionAnswered,
                some("2.1 Some(\"SQLite\")")
            ),
            ("parent".into(), EventKind::ChildRemoved, some("helper")),
            ("parent".into(), EventKind::ChildRemoved, some("fix-login")),
        ]
    );
    Ok(())
}

#[test]
fn a_workspace_s_creation_takes_the_base_recorded_after_it() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         INSERT INTO workspaces(id,repository_id,name,path,branch,state)
             VALUES ('w','repo','fix','/fix','fix','preparing');
         UPDATE workspaces SET base_commit='abc',base_ref='refs/heads/main' WHERE id='w';
         UPDATE workspaces SET base_ref='refs/heads/other' WHERE id='w';",
    )?;
    let created: Vec<_> = journal(&db)?
        .into_iter()
        .filter(|event| event.kind == crate::daemon::events::EventKind::Created)
        .map(|event| event.base_ref)
        .collect();
    assert_eq!(created, [Some("refs/heads/main".to_owned())]);
    Ok(())
}
