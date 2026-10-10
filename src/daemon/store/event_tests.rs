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
        crate::forge::link::ItemKind::Pr,
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
    // Ready-for-review marks and agent states record into the journal, so
    // they go with it.
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
    ] {
        db.execute_batch(&format!("DROP TRIGGER {trigger};"))?;
    }
    db.execute_batch("DROP TABLE workspace_events; ALTER TABLE workspaces DROP COLUMN observed_branch; ALTER TABLE workspace_completion DROP COLUMN cause;")?;
    // Migrations 31 to 35 add these columns again when the emulated database
    // upgrades.
    db.execute_batch("ALTER TABLE workspace_holds DROP COLUMN from_continuation;")?;
    db.execute_batch("ALTER TABLE executions DROP COLUMN scope_token;")?;
    db.execute_batch(
        "DROP INDEX workspaces_base; ALTER TABLE workspaces DROP COLUMN base_workspace_id;",
    )?;
    db.execute_batch("ALTER TABLE workspace_issue DROP COLUMN title;")?;
    Ok(())
}
