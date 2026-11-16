//! Lifecycle events must share the mutation's transaction and survive removal.
use super::*;
use shoal_ids::space_ids::SpaceId;

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
        "INSERT INTO workspaces(id,path,branch,state,observed_branch)
            VALUES ('w','/work','worker','preparing','worker');
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
         INSERT INTO workspaces(id,path,branch,state,observed_branch)
            VALUES ('w','/work','worker','preparing','worker');
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
fn link_events_include_item_kind_and_url() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO workspaces(id,path,branch,state,observed_branch)
            VALUES ('w','/work','worker','ready','worker');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','worker');
         COMMIT;",
    )?;
    crate::daemon::events::record_link(
        &db,
        &SpaceId::from("w"),
        crate::daemon::events::EventKind::Linked,
        crate::model::Relation::Assigned,
        crate::forge::items::link::ItemKind::Pr,
        "https://forge.example/team/repo/pulls/7",
    )?;
    let record: String = db.query_row(
        "SELECT record FROM space_events WHERE json_extract(record,'$.kind')='linked'",
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
    db.execute_batch(
        "BEGIN;
         INSERT INTO workspaces(id,path,branch,state,observed_branch)
            VALUES ('w','/work','worker','ready','worker');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','worker');
         COMMIT;",
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
        &SpaceId::from("w"),
        crate::forge::items::link::LinkKind::Pr,
        &update.url,
        &update,
    )?;
    let record: String = db.query_row(
        "SELECT record FROM space_events WHERE json_extract(record,'$.kind')='item_activity'",
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

/// Remove the current event schema when tests emulate an older database.
pub(super) fn remove_schema(db: &Connection) -> Result<()> {
    // Rebuilt tables go without deleting what references them, as in a
    // rebuilding migration.
    db.pragma_update(None, "foreign_keys", false)?;
    let removed = remove_schema_objects(db);
    db.pragma_update(None, "foreign_keys", true)?;
    removed
}

fn remove_schema_objects(db: &Connection) -> Result<()> {
    // One transaction syncs the whole removal once instead of every statement.
    let tx = db.unchecked_transaction()?;
    let db: &Connection = &tx;
    db.execute_batch(include_str!("chat_places_undo.sql"))?;
    db.execute_batch(include_str!("chat_turn_marks_undo.sql"))?;
    db.execute_batch(include_str!("space_sessions_undo.sql"))?;
    db.execute_batch(include_str!("agent_queue_sources_undo.sql"))?;
    db.execute_batch(include_str!("issue_agents_undo.sql"))?;
    db.execute_batch(include_str!("space_scopes_undo.sql"))?;
    db.execute_batch(include_str!("space_columns_undo.sql"))?;
    db.execute_batch(include_str!("space_holds_undo.sql"))?;
    db.execute_batch(include_str!("events_by_space_undo.sql"))?;
    db.execute_batch(include_str!("review_sessions_undo.sql"))?;
    db.execute_batch(include_str!("spaces_table_undo.sql"))?;
    db.execute_batch(include_str!("marks_without_heads_undo.sql"))?;
    db.execute_batch(include_str!("consumed_upgrades_undo.sql"))?;
    db.execute_batch(include_str!("removed_spaces_undo.sql"))?;
    db.execute_batch(include_str!("gate_undo.sql"))?;
    db.execute_batch(include_str!("removed_space_files_undo.sql"))?;
    db.execute_batch(include_str!("file_events.sql"))?;
    db.execute_batch(include_str!("queue_issue_labels_undo.sql"))?;
    db.execute_batch(include_str!("issue_assignees_undo.sql"))?;
    db.execute_batch(include_str!("space_pr_titles_undo.sql"))?;
    db.execute_batch(include_str!("space_turn_retries_undo.sql"))?;
    db.execute_batch(include_str!("space_messages_undo.sql"))?;
    db.execute_batch(include_str!("chat_removed_events_undo.sql"))?;
    db.execute_batch(include_str!("space_records_undo.sql"))?;
    db.execute_batch(include_str!("chat_executions_undo.sql"))?;
    db.execute_batch(include_str!("space_priority_undo.sql"))?;
    db.execute_batch(include_str!("execution_spaces_undo.sql"))?;
    db.execute_batch(include_str!("agent_stops_undo.sql"))?;
    db.execute_batch(include_str!("agent_turns_undo.sql"))?;
    db.execute_batch(include_str!("agent_queue_scopes_undo.sql"))?;
    // Agents sleep no more.
    db.execute_batch(
        "DROP TRIGGER workspace_sleep_removed; DROP TRIGGER chat_sleep_removed;
         DROP TABLE space_sleep;",
    )?;
    db.execute_batch(include_str!("space_completion_undo.sql"))?;
    // Spaces convert no more.
    db.execute_batch(
        "DROP TRIGGER workspace_converted; DROP TRIGGER chat_converted;
         DROP TRIGGER workspace_conversion_removed; DROP TRIGGER chat_conversion_removed;
         DROP TABLE space_conversions; ALTER TABLE chats DROP COLUMN converted;",
    )?;
    // Spaces keep no progress again.
    db.execute_batch(
        "DROP TRIGGER workspace_progress_removed; DROP TRIGGER chat_progress_removed;
         DROP TABLE space_progress;",
    )?;
    db.execute_batch(include_str!("space_events_undo.sql"))?;
    // Workspaces record no copies again.
    db.execute_batch(
        "ALTER TABLE workspaces DROP COLUMN copied_from;
         ALTER TABLE workspaces DROP COLUMN copied_from_checkpoint;
         ALTER TABLE workspaces DROP COLUMN starting_checkpoint;
         ALTER TABLE workspaces DROP COLUMN starting_index;",
    )?;
    // Local merges go unrecorded again.
    db.execute_batch("DROP TABLE merges;")?;
    // Spaces have no numbers again, nor a view that reads them; chats take
    // their triggers with them.
    db.execute_batch(
        "DROP TRIGGER workspace_numbered; DROP TRIGGER chat_started;
         DROP VIEW free_space_number; DROP VIEW space_numbers; DROP VIEW spaces;
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
        "ALTER TABLE workspaces DROP COLUMN narrowing;
         ALTER TABLE executions DROP COLUMN narrowing;",
    )?;
    db.execute_batch(
        "ALTER TABLE workspaces DROP COLUMN mode; ALTER TABLE executions DROP COLUMN mode;",
    )?;
    tx.commit()?;
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
    db.execute_batch(
        "BEGIN;
         INSERT INTO workspaces(id,path,branch,state,observed_branch)
            VALUES ('w','/work','worker','ready','worker');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','worker');
         COMMIT;",
    )?;
    let url = "https://forge.example/team/repo/actions/runs/2765";
    crate::daemon::events::record_link(
        &db,
        &SpaceId::from("w"),
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
    crate::daemon::events::record_item_activity(
        &db,
        &SpaceId::from("w"),
        LinkKind::Ci,
        url,
        &update,
    )?;
    crate::daemon::events::record_item(&db, &SpaceId::from("w"), LinkKind::Ci, url, "approve")?;
    let records = db
        .prepare("SELECT record FROM space_events ORDER BY id")?
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
        "BEGIN;
         INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         INSERT INTO chats(id,agent,started_at)
             VALUES ('c','claude',1);
         INSERT INTO spaces(id,kind,repository_id,name,mode)
             VALUES ('c','chat','repo','planner','chat');
         INSERT INTO space_review(space_id,url,kind) VALUES ('c','',NULL);
         COMMIT;",
    )?;
    crate::daemon::events::record_link(
        &db,
        &SpaceId::from("c"),
        EventKind::Linked,
        crate::model::Relation::Assigned,
        crate::forge::items::link::LinkKind::Issue,
        "https://forge.example/team/repo/issues/1",
    )?;
    db.execute_batch(
        "INSERT INTO space_completion(space_id,record)
             VALUES ('c','{\"head\":\"abc\",\"cleanup\":true,\"error\":null}');
         DELETE FROM space_completion WHERE space_id='c';",
    )?;
    let rows = db
        .prepare("SELECT space_id,record FROM space_events ORDER BY id")?
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
            EventKind::Created,
            EventKind::ReviewReady,
            EventKind::Linked,
            EventKind::Completed,
            EventKind::Undone
        ]
    );
    for (space, event) in &events {
        assert_eq!(*space, "c");
        assert_eq!(event.space_id.as_str(), "c");
        assert_eq!(event.space_kind, crate::model::SpaceKind::Chat);
        assert_eq!(event.name, "planner");
    }
    Ok(())
}

/// The journal's records, oldest first.
fn journal(db: &Connection) -> Result<Vec<crate::daemon::events::EventDetails>> {
    db.prepare("SELECT record FROM space_events ORDER BY id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|record| Ok(serde_json::from_str(&record?)?))
        .collect()
}

#[test]
fn renames_questions_and_children_reach_their_spaces_logs() -> Result<()> {
    use crate::daemon::events::EventKind;
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         INSERT INTO workspaces(id,path,branch,state)
             VALUES ('parent','/plan','plan','ready');
         INSERT INTO spaces(id,kind,repository_id,name)
             VALUES ('parent','workspace','repo','plan');
         INSERT INTO space_parents(space_id,parent_id) VALUES ('child','parent');
         INSERT INTO workspaces(id,path,branch,state)
             VALUES ('child','/fix','fix','ready');
         INSERT INTO spaces(id,kind,repository_id,name)
             VALUES ('child','workspace','repo','fix');
         INSERT INTO space_parents(space_id,parent_id) VALUES ('chat','parent');
         INSERT INTO chats(id,agent,started_at) VALUES ('chat','claude',1);
         INSERT INTO spaces(id,kind,repository_id,name,mode)
             VALUES ('chat','chat','repo','helper','chat');
         UPDATE spaces SET name='Fix login' WHERE id='child';
         UPDATE workspaces SET branch='fix/login' WHERE id='child';
         INSERT INTO questions(space_number,number,space_id,created_at,question,
                 recommendation,choices)
             VALUES (2,1,'child',1,'Which database?','SQLite','[]');
         UPDATE questions SET answer='SQLite',answered_at=2 WHERE number=1;
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
                    event.branch.map(String::from).unwrap_or_default()
                )),
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
            ("child".into(), EventKind::Renamed, some("Fix login fix")),
            (
                "child".into(),
                EventKind::Renamed,
                some("Fix login fix/login")
            ),
            ("child".into(), EventKind::QuestionAsked, some("2.1 None")),
            (
                "child".into(),
                EventKind::QuestionAnswered,
                some("2.1 Some(\"SQLite\")")
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
         INSERT INTO workspaces(id,path,branch,state)
             VALUES ('w','/fix','fix','preparing');
         INSERT INTO spaces(id,kind,repository_id,name)
             VALUES ('w','workspace','repo','fix');
         UPDATE workspaces SET base_commit='abc',base_ref='refs/heads/main' WHERE id='w';
         UPDATE workspaces SET base_ref='refs/heads/other' WHERE id='w';
         COMMIT;",
    )?;
    let created: Vec<_> = journal(&db)?
        .into_iter()
        .filter(|event| event.kind == crate::daemon::events::EventKind::Created)
        .map(|event| event.base_ref)
        .collect();
    assert_eq!(created, [Some("refs/heads/main".to_owned())]);
    Ok(())
}

#[test]
fn the_journal_names_every_space_it_recorded_by_its_id_and_kind() -> Result<()> {
    use crate::daemon::events::{EventDetails, FileEventDetails, ShoalEventDetails};
    let mut db = Connection::open_in_memory()?;
    migrate_to(&mut db, 183)?;
    let chat = r#""chat":{"id":"c","repository_id":"repo","name":"planner","agent":"claude","started_at":1}"#;
    let records = [
        r#"{"kind":"ready","workspace_id":"w","chat_id":null,"repository_id":"repo","name":"fix","path":"/fix","branch":"fix","cause":null,"error":null}"#.to_owned(),
        format!(r#"{{"kind":"chat_started",{chat}}}"#),
        r#"{"kind":"completed","workspace_id":null,"chat_id":"c","repository_id":"repo","name":"planner","path":null,"branch":null,"cause":null,"error":null}"#.to_owned(),
        format!(r#"{{"kind":"chat_renamed",{}}}"#, chat.replace("planner", "helper")),
        r#"{"kind":"file_added","workspace_id":null,"chat_id":"c","repository_id":"repo","file":{"id":1,"chat":"helper","source":"added","name":"notes.md","path":"/notes.md","bytes":1,"created_at":1}}"#.to_owned(),
        format!(r#"{{"kind":"chat_ended",{}}}"#, chat.replace("planner", "helper")),
        r#"{"kind":"daemon_started","version":"0.17.0"}"#.to_owned(),
        r#"{"kind":"agent_state","workspace_id":"w","repository_id":"repo","name":"fix","path":"/fix","branch":"fix","cause":null,"error":null,"agent_state":{"state":"idle"}}"#.to_owned(),
    ];
    for (time, record) in (10i64..).zip(&records) {
        db.execute(
            "INSERT INTO workspace_events(created_at,record) VALUES (?1,?2)",
            rusqlite::params![time, record],
        )?;
    }
    // Retention deleted the newest event, whose ID stays given.
    db.execute("DELETE FROM workspace_events WHERE id=8", [])?;
    migrate(&mut db)?;

    let rows = db
        .prepare("SELECT id,created_at,space_id,record FROM space_events ORDER BY id")?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, i64, Option<String>, String)>>>()?;
    let times: Vec<_> = rows.iter().map(|(id, time, ..)| (*id, *time)).collect();
    assert_eq!(times, (1..=7).map(|id| (id, id + 9)).collect::<Vec<_>>());
    for (_, _, space, record) in &rows {
        let value: serde_json::Value = serde_json::from_str(record)?;
        for gone in ["workspace_id", "chat_id", "chat"] {
            assert!(value.get(gone).is_none(), "{record}");
        }
        assert_eq!(value["space_id"].as_str(), space.as_deref(), "{record}");
    }
    let space =
        |index: usize| -> Result<EventDetails> { Ok(serde_json::from_str(&rows[index].3)?) };
    let summary: Vec<_> = [0, 1, 2, 3, 5]
        .into_iter()
        .map(|index| {
            let event = space(index)?;
            Ok((
                event.kind,
                event.space_id.to_string(),
                event.space_kind,
                event.name.to_string(),
            ))
        })
        .collect::<Result<_>>()?;
    use crate::{daemon::events::EventKind, model::SpaceKind};
    let chat = |kind, name: &str| (kind, "c".to_owned(), SpaceKind::Chat, name.to_owned());
    assert_eq!(
        summary,
        [
            (
                EventKind::Ready,
                "w".into(),
                SpaceKind::Workspace,
                "fix".into()
            ),
            chat(EventKind::Created, "planner"),
            chat(EventKind::Completed, "planner"),
            chat(EventKind::Renamed, "helper"),
            chat(EventKind::Removed, "helper"),
        ]
    );
    let created = space(1)?;
    assert_eq!(
        created.agent.map(|agent| agent.name.to_string()).as_deref(),
        Some("claude")
    );
    assert_eq!(
        (created.path, created.branch, created.base_ref),
        (None, None, None)
    );
    let file: FileEventDetails = serde_json::from_str(&rows[4].3)?;
    assert_eq!(
        (file.space_id.as_str(), file.space_kind),
        ("c", SpaceKind::Chat)
    );
    let shoal: ShoalEventDetails = serde_json::from_str(&rows[6].3)?;
    assert_eq!(shoal.version.as_deref(), Some("0.17.0"));

    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         BEGIN;
         INSERT INTO chats(id,agent,started_at) VALUES ('d','codex',1);
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('d','chat','repo','next');
         COMMIT;",
    )?;
    let newest: (i64, String) = db.query_row(
        "SELECT id,space_id FROM space_events ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(newest, (9, "d".to_owned()));
    Ok(())
}
