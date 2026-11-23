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
    db.execute_batch(include_str!("hub_ci_job_reruns_undo.sql"))?;
    db.execute_batch(include_str!("ci_job_rerun_causes_undo.sql"))?;
    db.execute_batch(include_str!("hub_sleeps_undo.sql"))?;
    db.execute_batch(include_str!("hub_package_starts_undo.sql"))?;
    db.execute_batch(include_str!("hub_kept_files_undo.sql"))?;
    db.execute_batch(include_str!("hub_repositories_undo.sql"))?;
    db.execute_batch(include_str!("shared_repositories_undo.sql"))?;
    db.execute_batch(include_str!("shared_items_undo.sql"))?;
    db.execute_batch(include_str!("review_findings_as_threads_undo.sql"))?;
    db.execute_batch(include_str!("turn_retries_outlive_executions_undo.sql"))?;
    db.execute_batch(include_str!("space_changes_undo.sql"))?;
    db.execute_batch(include_str!("shared_files_undo.sql"))?;
    db.execute_batch(include_str!("hub_drop_alerts_undo.sql"))?;
    db.execute_batch(include_str!("hub_takeovers_undo.sql"))?;
    db.execute_batch(include_str!("hub_packages_undo.sql"))?;
    db.execute_batch(include_str!("chat_marks_undo.sql"))?;
    db.execute_batch(include_str!("pending_moves_undo.sql"))?;
    db.execute_batch(include_str!("thread_code_undo.sql"))?;
    db.execute_batch(include_str!("moved_parents_undo.sql"))?;
    db.execute_batch(include_str!("hub_move_fences_undo.sql"))?;
    db.execute_batch(include_str!("space_moves_undo.sql"))?;
    db.execute_batch(include_str!("threads_undo.sql"))?;
    db.execute_batch(include_str!("ci_job_reruns_undo.sql"))?;
    db.execute_batch(include_str!("alerts_undo.sql"))?;
    db.execute_batch(include_str!("lifecycle_generations_undo.sql"))?;
    db.execute_batch(include_str!("demo_viewers_undo.sql"))?;
    db.execute_batch(include_str!("pending_plans_undo.sql"))?;
    db.execute_batch(include_str!("hub_presence_undo.sql"))?;
    db.execute_batch(include_str!("hub_forward_nonces_undo.sql"))?;
    db.execute_batch(include_str!("app_entries_undo.sql"))?;
    db.execute_batch(include_str!("agent_restarts_undo.sql"))?;
    db.execute_batch(include_str!("hub_routing_undo.sql"))?;
    db.execute_batch(include_str!("failed_launches_undo.sql"))?;
    db.execute_batch(include_str!("merge_run_pushes_undo.sql"))?;
    db.execute_batch(include_str!("hub_kept_numbers_undo.sql"))?;
    db.execute_batch(include_str!("hub_numbering_undo.sql"))?;
    db.execute_batch(include_str!("hub_numbers_undo.sql"))?;
    db.execute_batch(include_str!("hub_directory_undo.sql"))?;
    db.execute_batch(include_str!("removal_directories_undo.sql"))?;
    db.execute_batch(include_str!("hub_undo.sql"))?;
    db.execute_batch(include_str!("waiting_preparations_undo.sql"))?;
    db.execute_batch(include_str!("ci_job_durations_dropped_undo.sql"))?;
    db.execute_batch(include_str!("space_engagement_undo.sql"))?;
    db.execute_batch("ALTER TABLE workspaces ADD COLUMN git_dir_id TEXT;")?;
    db.execute_batch(include_str!("execution_subagents_undo.sql"))?;
    db.execute_batch(include_str!("space_wake_timers_undo.sql"))?;
    db.execute_batch(include_str!("queue_starts_at_once_undo.sql"))?;
    db.execute_batch(include_str!("pending_worktrees_undo.sql"))?;
    db.execute_batch(include_str!("merge_runs_undo.sql"))?;
    db.execute_batch(include_str!("event_spaces_undo.sql"))?;
    db.execute_batch(include_str!("space_scope_agents_undo.sql"))?;
    db.execute_batch(include_str!("agent_assigned_issues_undo.sql"))?;
    db.execute_batch(include_str!("container_projects_undo.sql"))?;
    db.execute_batch(include_str!("triggers_undo.sql"))?;
    db.execute_batch(include_str!("lease_acquirers_undo.sql"))?;
    db.execute_batch(include_str!("space_creation_times_undo.sql"))?;
    db.execute_batch(include_str!("chat_agents_undo.sql"))?;
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
fn stored_chat_events_lose_their_path_and_branch() -> Result<()> {
    use shoal_protocol::events::EventSpace;
    let mut db = Connection::open_in_memory()?;
    migrate_to(&mut db, 211)?;
    let chat = serde_json::json!({"kind": "renamed", "space_id": "c", "space_kind": "chat",
        "repository_id": null, "name": "plan", "cause": null, "error": null});
    let workspace = serde_json::json!({"kind": "branch_changed", "space_id": "w",
        "space_kind": "workspace", "repository_id": "repo", "name": "fix", "path": "/fix",
        "branch": null, "cause": null, "error": null});
    let mut earlier_chat = chat.clone();
    earlier_chat["path"] = serde_json::Value::Null;
    earlier_chat["branch"] = serde_json::Value::Null;
    for (space, record) in [("c", &earlier_chat), ("w", &workspace)] {
        db.execute(
            "INSERT INTO space_events(space_id,record) VALUES (?1,?2)",
            rusqlite::params![space, record.to_string()],
        )?;
    }
    migrate(&mut db)?;
    let records = db
        .prepare("SELECT record FROM space_events ORDER BY id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|record| Ok(serde_json::from_str(&record?)?))
        .collect::<Result<Vec<serde_json::Value>>>()?;
    assert_eq!(records, [chat, workspace]);
    let spaces: Vec<_> = journal(&db)?.into_iter().map(|event| event.space).collect();
    assert_eq!(
        spaces,
        [
            EventSpace::Chat,
            EventSpace::Workspace {
                path: "/fix".into(),
                branch: None
            }
        ]
    );
    Ok(())
}

#[test]
fn a_stored_job_end_takes_the_reason_shoal_ended_it_for() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate_to(&mut db, 249)?;
    let ended = serde_json::json!({"kind": "job_ended", "space_id": "w",
        "space_kind": "workspace", "repository_id": "repo", "name": "job", "path": "/job",
        "branch": null, "cause": null, "error": null,
        "job": {"name": "rust", "run": "https://forge.example/team/repo/actions/runs/2765",
            "stopped": []}});
    db.execute(
        "INSERT INTO space_events(space_id,record) VALUES ('w',?1)",
        [ended.to_string()],
    )?;
    migrate(&mut db)?;
    let job = journal(&db)?.remove(0).job.unwrap();
    assert_eq!(job.reason, "its runner outlived its finished run");
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

#[test]
fn the_journal_names_every_space_it_recorded_by_its_id_and_kind() -> Result<()> {
    use shoal_protocol::events::{EventDetails, FileEventDetails, ShoalEventDetails};
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
                event.space.kind(),
                event.name.to_string(),
            ))
        })
        .collect::<Result<_>>()?;
    use crate::model::SpaceKind;
    use shoal_protocol::events::EventKind;
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
        (created.space, created.base_ref),
        (shoal_protocol::events::EventSpace::Chat, None)
    );
    let file: FileEventDetails = serde_json::from_str(&rows[4].3)?;
    assert_eq!(
        (file.file.space.id.as_str(), file.file.space.kind),
        ("c", SpaceKind::Chat)
    );
    let shoal: ShoalEventDetails = serde_json::from_str(&rows[6].3)?;
    assert_eq!(shoal.version.as_deref(), Some("0.17.0"));

    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         BEGIN;
         INSERT INTO chats(id) VALUES ('d');
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
