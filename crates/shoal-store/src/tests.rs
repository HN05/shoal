use super::*;
use shoal_ids::space_ids::SpaceId;

/// A database with the current schema that holds the repository `repo`,
/// its workspace `workspace` and that workspace's running execution
/// `execution`.
fn seeded() -> Result<Connection> {
    let db = crate::test_support::migrated()?;
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
        INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        BEGIN;
        INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
            VALUES ('workspace','/work','worker','ready','abc','/admin');
        INSERT INTO spaces(id,kind,repository_id,name)
            VALUES ('workspace','workspace','repo','worker');
        COMMIT;
        INSERT INTO executions(id,space_id,state) VALUES ('execution','workspace','running');",
    )?;
    Ok(db)
}

#[test]
fn new_spaces_take_the_smallest_number_no_space_holds() -> Result<()> {
    let db = seeded()?;
    db.execute_batch(
        "INSERT INTO spaces(id,kind,repository_id,name)
             VALUES ('second','workspace','repo','second');
         INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir) VALUES ('second','/second','second','ready','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name,mode)
             VALUES ('chat','chat','repo','chat','chat');
         INSERT INTO chats(id) VALUES ('chat');
         DELETE FROM spaces WHERE id='second';
         INSERT INTO spaces(id,kind,repository_id,name,mode)
             VALUES ('reused','chat','repo','reused','chat'),('next','chat','repo','next','chat');
         INSERT INTO chats(id) VALUES ('reused'),('next');
         UPDATE spaces SET name='renamed' WHERE id='workspace';",
    )?;
    assert_eq!(
        space_numbers(&db)?,
        [
            ("chat".into(), 3),
            ("next".into(), 4),
            ("renamed".into(), 1),
            ("reused".into(), 2)
        ]
    );
    Ok(())
}

/// Every space's name and number, by name.
fn space_numbers(db: &Connection) -> Result<Vec<(String, i64)>> {
    Ok(db
        .prepare("SELECT name,number FROM spaces ORDER BY name")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

#[test]
fn mappers_use_named_columns() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute_batch(
        r#"INSERT INTO repositories(id,path,source,last_used,name,workspaces_dir)
                VALUES ('repo','/checkout','origin',42,'project','/workspaces');
            INSERT INTO spaces(id,kind,repository_id,name)
                VALUES ('workspace','workspace','repo','worker');
            INSERT INTO workspaces(id,path,branch,state,error,
                base_commit,base_ref,git_dir,setup_finished)
                VALUES ('workspace','/work','feature','failed','setup error',
                    'abc','refs/heads/main','/git/worktrees/worker',1);
            INSERT INTO ports(workspace_id,name,port,env_var,reason)
                VALUES ('workspace','web',12345,'WEB_PORT',NULL);
            INSERT INTO executions(id,space_id,state,wrapper,child,group_id,agent)
                VALUES ('execution','workspace','unknown','{"pid":123,"birth":"wrapper"}',
                    '{"pid":456,"birth":"child"}',456,'claude');"#,
    )?;
    check_columns(
        &db,
        "repositories",
        REPOSITORY_COLUMNS,
        repository,
        serde_json::json!({
            "id": "repo", "path": "/checkout", "source": "origin", "last_used": 42,
            "name": "project", "workspaces_dir": "/workspaces"
        }),
    )?;
    check_columns(
        &db,
        "spaces JOIN workspaces USING(id)",
        WORKSPACE_COLUMNS,
        workspace,
        serde_json::json!({
            "id": "workspace", "kind": "workspace", "number": 1, "repository_id": "repo",
            "name": "worker", "path": "/work",
            "workdir": {"kind": "worktree", "path": "/work",
                        "workspace": {"id": "workspace", "name": "worker", "branch": "feature"}},
            "branch": "feature", "state": "failed", "error": "setup error",
            "checkout": {"state": "created",
                         "base": {"commit": "abc", "reference": "refs/heads/main"},
                         "git_dir": "/git/worktrees/worker"},
            "base_workspace": null, "parent": null, "links": {"issues": [], "prs": [], "opened": [], "related": []}, "running": false, "done": false,
            "setup_skipped": false, "disposable": false
        }),
    )?;
    check_columns(
        &db,
        "ports",
        PORT_COLUMNS,
        port,
        serde_json::json!({
            "workspace_id": "workspace", "name": "web", "port": 12345,
            "env_var": "WEB_PORT", "reason": null
        }),
    )?;
    check_columns(
        &db,
        "executions",
        EXECUTION_COLUMNS,
        execution,
        serde_json::json!({
            "id": "execution", "space_id": "workspace", "state": "unknown",
            "wrapper": {"pid": 123, "birth": "wrapper"},
            "child": {"pid": 456, "birth": "child"}, "group_id": 456,
            "agent": "claude"
        }),
    )?;
    assert!(setup_finished(&db, &SpaceId::from("workspace"))?);
    Ok(())
}

fn check_columns<T: Serialize>(
    db: &Connection,
    table: &str,
    columns: &str,
    mapper: fn(&Row<'_>) -> rusqlite::Result<T>,
    expected: serde_json::Value,
) -> Result<()> {
    // Exercise the shared projection and a reordered projection with an
    // unrelated leading column, as a changed schema or query might supply.
    let reordered = format!(
        "'extra' AS unrelated,{}",
        columns.split(',').rev().collect::<Vec<_>>().join(",")
    );
    for projection in [columns, &reordered] {
        let record = db.query_row(&format!("SELECT {projection} FROM {table}"), [], mapper)?;
        assert_eq!(serde_json::to_value(record)?, expected);
    }
    Ok(())
}

#[tokio::test]
async fn approval_records_survive_reopen_and_follow_workspace_ownership() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await.unwrap();
    store.run(|db| {
            db.execute_batch("INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                BEGIN;
                INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir) VALUES ('workspace','/work','worker','ready','abc','/admin');
                INSERT INTO spaces(id,kind,repository_id,name) VALUES ('workspace','workspace','repo','worker');
                COMMIT;")?;
            Ok(())
        }).await.unwrap();
    let reopened = Store::open(path.clone()).await.unwrap();
    reopened.run(|db| {
            db.execute("INSERT INTO access_requests(id,space_id,target_key,name,record,active,number) VALUES ('request','workspace','pool','default','{}',1,1)", [])?;
            assert!(db.execute("INSERT INTO access_requests(id,space_id,target_key,name,record,active,number) VALUES ('duplicate','workspace','pool','default','{}',1,2)", []).is_err());
            assert!(db.execute("INSERT INTO access_requests(id,space_id,target_key,name,record,active,number) VALUES ('orphan','missing','pool','default','{}',1,3)", []).is_err());
            assert!(db.execute("INSERT INTO access_requests(id,space_id,target_key,name,record,active,number) VALUES ('renumbered','workspace','pool','other','{}',1,1)", []).is_err());
            Ok(())
        }).await.unwrap();
    Store::open(path)
        .await
        .unwrap()
        .run(|db| {
            let count = |db: &Connection| {
                db.query_row("SELECT COUNT(*) FROM access_requests", [], |row| {
                    row.get::<_, u32>(0)
                })
            };
            assert_eq!(count(db)?, 1);
            db.execute("UPDATE workspaces SET state='failed'", [])?;
            assert_eq!(count(db)?, 1);
            db.execute("DELETE FROM spaces", [])?;
            assert_eq!(count(db)?, 0);
            Ok(())
        })
        .await
        .unwrap();
}

#[test]
fn a_held_thread_records_its_events_only_once_released() -> Result<()> {
    let db = seeded()?;
    let events = || -> Result<Vec<String>> {
        Ok(db
            .prepare(
                "SELECT json_extract(record,'$.kind') FROM space_events
                    WHERE json_extract(record,'$.kind') LIKE 'thread_%' ORDER BY id",
            )?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    };
    db.execute_batch(
        "INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener,
                holder_id,review_id,held)
            VALUES (1,'workspace',1,1,'review','Unclear name',1,'user','workspace',1,1);
        INSERT INTO thread_posts(thread_id,created_at,side,sender,text)
            VALUES (1,2,'opener','user','Call it retries');",
    )?;
    assert!(events()?.is_empty());
    db.execute("UPDATE threads SET held=0 WHERE id=1", [])?;
    assert_eq!(events()?, ["thread_opened", "thread_posted"]);
    Ok(())
}

/// Seed a workspace in each state, an execution and a simulator clean
/// request, as a daemon that stopped mid-operation leaves them.
fn seed_interrupted_operations(db: &mut Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);",
    )?;
    for state in WorkspaceState::ALL {
        db.execute(
            "INSERT INTO spaces(id,kind,repository_id,name) VALUES (?1,'workspace','repo',?1)",
            [state],
        )?;
        db.execute(
            "INSERT INTO workspaces(id,path,branch,state,error,base_commit,git_dir)
                VALUES (?1,?1,?1,?1,'original error','abc','/admin')",
            [state],
        )?;
    }
    db.execute_batch("INSERT INTO executions(id,space_id,state) VALUES ('execution','preparing','running');
            INSERT INTO simulator_clean_requests(request_id,workspace_id,record)
                VALUES ('clean','preparing','{\"status\":\"requested\",\"reason\":\"keep audit\"}');")?;
    Ok(())
}

fn operation_snapshot(db: &mut Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare(
            "SELECT json_array(id,state,error) FROM workspaces
            UNION ALL SELECT json_array(id,state) FROM executions
            UNION ALL SELECT record FROM simulator_clean_requests ORDER BY 1",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[tokio::test]
async fn current_schema_reopen_preserves_active_operations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await.unwrap();
    store.run(seed_interrupted_operations).await.unwrap();
    let before = store.run(operation_snapshot).await.unwrap();
    for _ in 0..2 {
        let reopened = Store::open(path.clone()).await.unwrap();
        assert_eq!(reopened.run(operation_snapshot).await.unwrap(), before);
    }
}
