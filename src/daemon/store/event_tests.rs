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
fn retention_is_bounded_and_ids_are_never_reused() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    let tx = db.transaction()?;
    for _ in 0..1005 {
        tx.execute("INSERT INTO workspace_events(record) VALUES ('{}')", [])?;
    }
    tx.commit()?;
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM workspace_events", [], |r| r
            .get::<_, i64>(0))?,
        1000
    );
    assert_eq!(
        db.query_row("SELECT MIN(id) FROM workspace_events", [], |r| r
            .get::<_, i64>(0))?,
        6
    );
    db.execute("DELETE FROM workspace_events", [])?;
    db.execute("INSERT INTO workspace_events(record) VALUES ('{}')", [])?;
    assert_eq!(db.last_insert_rowid(), 1006);
    Ok(())
}

/// Remove the current event schema when tests emulate an older database.
pub(super) fn remove_schema(db: &Connection) -> Result<()> {
    // Ready-for-review marks, agent states and chats record into the
    // journal, so they go with it.
    db.execute_batch(
        "DROP TRIGGER workspace_chat_changed; DROP INDEX workspaces_chat;
         ALTER TABLE workspaces DROP COLUMN chat_id; DROP TABLE chats;",
    )?;
    db.execute_batch("DROP TABLE workspace_review; DROP TABLE workspace_agent_state;")?;
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
        "workspace_parent_removed",
        "demo_started",
        "demo_stopped",
        "simulator_acquired",
        "simulator_released",
        "simulator_deleted",
    ] {
        db.execute_batch(&format!("DROP TRIGGER {trigger};"))?;
    }
    db.execute_batch("DROP TABLE workspace_events; ALTER TABLE workspaces DROP COLUMN observed_branch; ALTER TABLE workspace_completion DROP COLUMN cause;")?;
    // Later migrations add these columns and tables again when the emulated
    // database upgrades.
    db.execute_batch("ALTER TABLE workspace_holds DROP COLUMN from_continuation;")?;
    db.execute_batch("ALTER TABLE notifications DROP COLUMN workspace_id;")?;
    db.execute_batch("ALTER TABLE notifications DROP COLUMN subject;")?;
    db.execute_batch("ALTER TABLE chat_messages DROP COLUMN workspace_id;")?;
    db.execute_batch("ALTER TABLE executions DROP COLUMN scope_token;")?;
    db.execute_batch("ALTER TABLE executions DROP COLUMN agent;")?;
    db.execute_batch(
        "DROP INDEX workspaces_base; ALTER TABLE workspaces DROP COLUMN base_workspace_id;",
    )?;
    db.execute_batch(
        "DROP INDEX workspaces_parent; ALTER TABLE workspaces DROP COLUMN parent_workspace_id;",
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
    db.execute_batch("ALTER TABLE agent_messages DROP COLUMN attachments;")?;
    db.execute_batch("ALTER TABLE agent_messages DROP COLUMN wakes;")?;
    db.execute_batch("DROP TABLE workspace_files;")?;
    db.execute_batch("DROP TABLE earlier_attached_files;")?;
    Ok(())
}
