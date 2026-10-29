use super::*;

// Reconstruct old schemas from a frozen pre-refactor snapshot, independently
// of MIGRATIONS, so missing or misnumbered steps cannot fix their own fixture.
fn historical_database(version: i64) -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch("PRAGMA foreign_keys=ON;")?;
    if version == 0 {
        return Ok(db);
    }
    db.execute_batch(include_str!("../../../tests/fixtures/schema_v17.sql"))?;
    if version >= 18 {
        db.execute_batch(
            "CREATE INDEX executions_workspace ON executions(workspace_id);
                CREATE INDEX workspaces_repository ON workspaces(repository_id);",
        )?;
    }
    if version >= 19 {
        db.execute_batch(
            "CREATE INDEX IF NOT EXISTS access_request_target
                ON access_requests(workspace_id,target_key);",
        )?;
    }
    if version < 4 {
        db.execute_batch("DROP INDEX repository_names;")?;
    }
    for (introduced, table, column) in [
        (16, "workspaces", "setup_finished"),
        (13, "repositories", "workspaces_dir"),
        (10, "executions", "group_id"),
        (10, "executions", "child"),
        (10, "executions", "wrapper"),
        (10, "workspaces", "git_dir_id"),
        (10, "workspaces", "git_dir"),
        (9, "resource_leases", "mode"),
        (5, "ports", "reason"),
        (4, "repositories", "name"),
        (2, "workspaces", "base_ref"),
        (2, "workspaces", "base_commit"),
    ] {
        if version < introduced {
            db.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column};"))?;
        }
    }
    for (introduced, table) in [
        (17, "access_requests"),
        (15, "notifications"),
        (14, "pr_cleanup"),
        (12, "repository_configs"),
        (11, "repository_removals"),
        (8, "resource_leases"),
        (8, "resource_pools"),
        (7, "simulator_clean_requests"),
        (6, "simulators"),
        (3, "ports"),
    ] {
        if version < introduced {
            db.execute_batch(&format!("DROP TABLE {table};"))?;
        }
    }
    if version >= 29 {
        db.execute_batch(
            "CREATE TABLE workspace_renames (
                    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
                    name TEXT NOT NULL UNIQUE, branch TEXT NOT NULL
                );",
        )?;
    }
    if version >= 24 {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS workspace_continuation (
                    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE
                );",
        )?;
    }
    if version >= 26 {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS workspace_scopes (
                    token TEXT PRIMARY KEY,
                    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE
                );",
        )?;
    }
    if version >= 27 {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS workspace_holds (
                    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                    name TEXT NOT NULL, reason TEXT, created_at INTEGER NOT NULL,
                    PRIMARY KEY(workspace_id,name)
                );",
        )?;
    }
    if version >= 25 {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS pr_activity (
                    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                    url TEXT NOT NULL, record TEXT NOT NULL,
                    PRIMARY KEY(workspace_id,url)
                );",
        )?;
    }
    if version >= 22 {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS workspace_issue (
                    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
                    url TEXT NOT NULL, error TEXT
                );",
        )?;
    }
    if version >= 23 {
        db.execute_batch(
                "CREATE TABLE repository_resource_leases (
                    lease_id TEXT PRIMARY KEY REFERENCES resource_leases(id) ON DELETE CASCADE,
                    repository_id TEXT NOT NULL REFERENCES repositories(id), path TEXT NOT NULL
                );
                CREATE INDEX repository_resource_owner ON repository_resource_leases(repository_id);",
            )?;
    }
    if version >= 21 {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS workspace_completion (
                workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
                record TEXT NOT NULL
            );",
        )?;
    }
    if version >= 20 {
        db.execute_batch(include_str!("simulator_owner.sql"))?;
    }
    if version >= 28 {
        db.execute_batch(include_str!("workspace_events.sql"))?;
    }
    if version >= 30 {
        db.execute_batch(include_str!("workspace_review.sql"))?;
    }
    if version >= 31 {
        db.execute_batch(include_str!("workspace_undone.sql"))?;
    }
    if version >= 32 {
        db.execute_batch("ALTER TABLE executions ADD COLUMN scope_token TEXT;")?;
    }
    if version >= 33 {
        db.execute_batch(include_str!("workspace_base.sql"))?;
    }
    if version >= 35 {
        db.execute_batch("ALTER TABLE workspace_issue ADD COLUMN title TEXT;")?;
    }
    if version >= 36 {
        db.execute_batch(include_str!("agent_messages.sql"))?;
    }
    if version >= 41 {
        db.execute_batch(include_str!("questions.sql"))?;
    }
    if version >= 37 {
        db.execute_batch(include_str!("workspace_agent_state.sql"))?;
    }
    if version >= 38 {
        db.execute_batch(include_str!("agent_queue.sql"))?;
    }
    if version >= 39 {
        db.execute_batch(
            "ALTER TABLE repository_removals ADD COLUMN workspace_paths TEXT NOT NULL DEFAULT '[]';
                 CREATE TABLE IF NOT EXISTS workspace_item_activity (
                    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                    url TEXT NOT NULL,
                    record TEXT NOT NULL,
                    PRIMARY KEY(workspace_id,url)
                );",
        )?;
    }
    if version >= 42 {
        db.execute_batch(include_str!("chats.sql"))?;
    }
    if version >= 44 {
        db.execute_batch("ALTER TABLE workspaces ADD COLUMN active_at INTEGER;")?;
    }
    if version >= 45 {
        db.execute_batch(REMOVED_WORKSPACES)?;
    }
    if version >= 46 {
        db.execute_batch(include_str!("display_numbers.sql"))?;
    }
    if version >= 47 {
        db.execute_batch(include_str!("workspace_items.sql"))?;
    }
    if version >= 48 {
        db.execute_batch(include_str!("agent_queue_restores.sql"))?;
    }
    if version >= 49 {
        db.execute_batch(CLEANUP_GRACE)?;
    }
    if version >= 51 {
        db.execute_batch(CHECKPOINTS)?;
    }
    if version >= 53 {
        db.execute_batch(KEPT_BRANCHES)?;
    }
    if version >= 54 {
        db.execute_batch("ALTER TABLE questions ADD COLUMN closed_at INTEGER;")?;
    }
    if version >= 55 {
        db.execute_batch(include_str!("workspace_parent.sql"))?;
    }
    if version >= 56 {
        db.execute_batch(QUEUE_TARGETS)?;
    }
    if version >= 57 {
        db.execute_batch(include_str!("chat_messages.sql"))?;
    }
    if version >= 58 {
        db.execute_batch(include_str!("repository_approvals.sql"))?;
    }
    if version >= 59 {
        db.execute_batch(QUESTION_WITHDRAWN)?;
    }
    if version >= 60 {
        db.execute_batch(NOTIFICATION_SENDERS)?;
    }
    if version >= 61 {
        db.execute_batch(CHAT_MESSAGE_SENDERS)?;
    }
    if version >= 62 {
        db.execute_batch(NOTIFICATION_SUBJECTS)?;
    }
    if version >= 63 {
        db.execute_batch(include_str!("agent_queue_dependencies.sql"))?;
    }
    if version >= 64 {
        db.execute_batch(FORGE_HOSTS)?;
    }
    if version >= 65 {
        db.execute_batch(REPOSITORY_AGENT_ENVIRONMENTS)?;
    }
    if version >= 66 {
        db.execute_batch(REPOSITORY_CONTRIBUTIONS)?;
    }
    if version >= 67 {
        db.execute_batch(REPOSITORY_MOVES)?;
    }
    if version >= 69 {
        db.execute_batch(DEMOS)?;
    }
    if version >= 70 {
        db.execute_batch(include_str!("workspace_turn_retries.sql"))?;
    }
    if version >= 71 {
        chat_names::name_chats(&db)?;
        db.execute_batch(include_str!("chat_names.sql"))?;
    }
    if version >= 72 {
        db.execute_batch(include_str!("agent_queue_stopped_restores.sql"))?;
    }
    if version >= 73 {
        db.execute_batch(include_str!("session_cleanup.sql"))?;
    }
    if version >= 74 {
        db.execute_batch(include_str!("execution_upstream_resets.sql"))?;
    }
    if version >= 75 {
        db.execute_batch(AUTOMATIC_RESTORES)?;
    }
    if version >= 76 {
        db.execute_batch(include_str!("places.sql"))?;
    }
    if version >= 77 {
        db.execute_batch(include_str!("owned_places.sql"))?;
    }
    if version >= 78 {
        db.execute_batch(EXECUTION_AGENTS)?;
    }
    if version >= 79 {
        db.execute_batch(include_str!("execution_unretried_turns.sql"))?;
    }
    db.pragma_update(None, "user_version", version)?;
    db.execute_batch(
            "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
            INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                VALUES ('workspace','repo','worker','/work','worker','ready');
            INSERT INTO executions(id,workspace_id,state) VALUES ('execution','workspace','running');",
        )?;
    Ok(db)
}

fn schema_snapshot(db: &Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")?
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|sql| Ok(sql?.split_whitespace().collect()))
        .collect::<rusqlite::Result<_>>()?)
}

#[test]
fn migrates_every_recorded_version_to_the_same_schema() -> Result<()> {
    let expected = schema_snapshot(&historical_database(SCHEMA_VERSION)?)?;
    assert_eq!(MIGRATIONS.last().unwrap().0, SCHEMA_VERSION);
    for version in 0..=SCHEMA_VERSION {
        let mut db = historical_database(version)?;
        migrate(&mut db).with_context(|| format!("upgrade from version {version}"))?;
        assert_eq!(schema_snapshot(&db)?, expected, "from version {version}");
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            SCHEMA_VERSION
        );
        if version > 0 {
            assert_eq!(
                executions(&db, "workspace")?[0].state,
                ExecutionState::Running
            );
            assert_eq!(
                db.query_row("SELECT path FROM workspaces", [], |r| r.get::<_, String>(0))?,
                "/work"
            );
        }
        migrate(&mut db)?;
        assert_eq!(schema_snapshot(&db)?, expected);
    }
    Ok(())
}

#[test]
fn migration_names_recorded_chats_and_keeps_what_they_own() -> Result<()> {
    let mut db = historical_database(70)?;
    db.execute_batch(
        "INSERT INTO workspaces(id,repository_id,name,path,branch,state)
             VALUES ('taken','repo','repo-chat-2','/taken','taken','ready');
         INSERT INTO chats(id,repository_id,number,agent,pid,birth,started_at)
             VALUES ('first','repo',1,'claude',1,'b',1),('second','repo',2,'codex',2,'b',2);
         UPDATE workspaces SET chat_id='second' WHERE id='workspace';
         INSERT INTO chat_messages(chat_id,created_at,workspace,message)
             VALUES ('second',1,'worker','hello');",
    )?;
    let count = |db: &Connection, table: &str| -> Result<i64> {
        Ok(db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
    };
    let events = count(&db, "workspace_events")?;
    migrate(&mut db)?;

    let names = db
        .prepare("SELECT id,name FROM chats ORDER BY id")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        names,
        [
            ("first".into(), "repo-chat".into()),
            ("second".into(), "repo-chat-3".into())
        ]
    );
    assert_eq!(
        db.query_row(
            "SELECT chat_id FROM workspaces WHERE id='workspace'",
            [],
            |r| { r.get::<_, String>(0) }
        )?,
        "second"
    );
    assert_eq!(count(&db, "chat_messages")?, 1);
    // The journal gains no events and names its chats as the rows do.
    assert_eq!(count(&db, "workspace_events")?, events);
    let started = db
        .prepare(
            "SELECT json_extract(record,'$.chat.name'),json_type(record,'$.chat.number')
             FROM workspace_events WHERE json_extract(record,'$.kind')='chat_started' ORDER BY id",
        )?
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        started,
        [("repo-chat".into(), None), ("repo-chat-3".into(), None)]
    );
    Ok(())
}

#[test]
fn queued_launches_keep_their_order_as_typed_entries() -> Result<()> {
    let mut db = historical_database(47)?;
    db.execute_batch(
        "INSERT INTO agent_queue(seq,id,repository_id,name,agent,plan,env,state,created_at)
                VALUES (7,'later','repo','later','claude','{}','{}','queued',1),
                       (3,'first','repo','first','claude','{}','{}','queued',1);",
    )?;
    migrate(&mut db)?;
    let entries = db
        .prepare("SELECT id,kind FROM agent_queue ORDER BY seq")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        entries,
        [
            ("first".into(), "launch".into()),
            ("later".into(), "launch".into())
        ]
    );
    Ok(())
}

#[test]
fn restores_outlive_their_execution_and_launches_keep_their_dependencies() -> Result<()> {
    let mut db = historical_database(71)?;
    db.execute_batch(
        "INSERT INTO agent_queue(seq,id,kind,repository_id,name,agent,plan,env,state,created_at)
                VALUES (4,'launch','launch','repo','later','claude','{}','{}','queued',1);
         INSERT INTO agent_queue_dependencies(entry_id,kind,target)
                VALUES ('launch','workspace','worker');
         INSERT INTO agent_queue(seq,id,kind,repository_id,name,agent,execution_id,state,created_at,workspace_id)
                VALUES (2,'restore','restore','repo','worker','claude','execution','queued',1,'workspace');",
    )?;
    migrate(&mut db)?;
    let count = |sql: &str| -> Result<i64> { Ok(db.query_row(sql, [], |r| r.get(0))?) };
    assert_eq!(count("SELECT COUNT(*) FROM agent_queue_dependencies")?, 1);
    db.execute("DELETE FROM executions WHERE id='execution'", [])?;
    assert_eq!(count("SELECT COUNT(*) FROM agent_queue")?, 2);
    // A restore the daemon starts carries its terminal's variables.
    db.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,env,execution_id,state,created_at,workspace_id)
            VALUES ('started','restore','repo','worker','claude','{}','ended','queued',1,'workspace')",
        [],
    )?;
    Ok(())
}

#[test]
fn continued_workspaces_keep_their_protection_as_holds() -> Result<()> {
    let mut db = historical_database(30)?;
    db.execute(
        "INSERT INTO workspace_continuation(workspace_id) VALUES ('workspace')",
        [],
    )?;
    migrate(&mut db)?;
    let hold: (String, String, bool) = db.query_row(
        "SELECT name,reason,from_continuation FROM workspace_holds WHERE workspace_id='workspace'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(
        hold,
        (
            "continue".into(),
            "Converted from an earlier Shoal version".into(),
            true
        )
    );
    Ok(())
}

#[test]
fn migrated_hold_reason_no_longer_names_the_retired_command() -> Result<()> {
    let mut db = historical_database(33)?;
    db.execute_batch(
        "INSERT INTO workspace_holds(workspace_id,name,reason,created_at,from_continuation)
                VALUES ('workspace','continue','Kept by shoal continue',1,1);
            INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                VALUES ('other','repo','other','/other','other','ready');
            INSERT INTO workspace_holds(workspace_id,name,reason,created_at,from_continuation)
                VALUES ('other','thread','Kept by shoal continue',1,0);",
    )?;
    migrate(&mut db)?;
    let reasons = db
        .prepare("SELECT reason FROM workspace_holds ORDER BY workspace_id")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // A user hold with the same reason keeps it.
    assert_eq!(
        reasons,
        [
            "Kept by shoal continue",
            "Converted from an earlier Shoal version"
        ]
    );
    Ok(())
}

#[test]
fn removal_snapshots_become_checkpoints_their_records_name() -> Result<()> {
    let mut db = historical_database(50)?;
    db.execute(
        "INSERT INTO removed_workspaces(workspace_id,repository_id,name,expires_at,record)
                VALUES ('gone','repo','topic',200,?1)",
        [serde_json::json!({
            "workspace_id": "gone",
            "repository_id": "repo",
            "name": "topic",
            "branch": "topic",
            "head": "aaaa",
            "snapshot": "bbbb",
            "files_omitted": true,
            "base_ref": null,
            "base_commit": null,
            "links": {"issue": null, "prs": []},
            "cause": "manual",
            "removed_at": 100,
            "expires_at": 200,
        })
        .to_string()],
    )?;
    migrate(&mut db)?;
    let checkpoint: String = db.query_row(
        "SELECT record FROM checkpoints WHERE workspace_id='gone' AND number=1",
        [],
        |r| r.get(0),
    )?;
    let checkpoint: crate::model::Checkpoint = serde_json::from_str(&checkpoint)?;
    assert_eq!(
        checkpoint,
        crate::model::Checkpoint {
            workspace_id: "gone".into(),
            repository_id: "repo".into(),
            number: 1,
            name: None,
            cause: crate::model::CheckpointCause::Removal,
            branch: Some("topic".into()),
            head: "aaaa".into(),
            commit: "bbbb".into(),
            files_omitted: true,
            created_at: 100,
            expires_at: 200,
        }
    );
    let removed: String =
        db.query_row("SELECT record FROM removed_workspaces", [], |r| r.get(0))?;
    let removed: crate::model::RemovedWorkspace = serde_json::from_str(&removed)?;
    assert_eq!(removed.checkpoint, checkpoint);
    let last: i64 = db.query_row(
        "SELECT last FROM checkpoint_numbers WHERE workspace_id='gone'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(last, 1);
    Ok(())
}

#[test]
fn migrated_notifications_name_the_numbered_commands() -> Result<()> {
    let mut db = historical_database(39)?;
    db.execute_batch(
            "INSERT INTO notifications(created_at,workspace,kind,message) VALUES
                (1,'w','access_requested','access 7: port/web / web: serve; approve with shoal access approve 7'),
                (2,'w','agent_message','run shoal access approve 7'),
                (3,'w','question_asked','question 4 (answer with shoal answer 4): Which database?');",
        )?;
    migrate(&mut db)?;
    let rows = db
        .prepare("SELECT kind,message FROM notifications ORDER BY id")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        rows,
        [
            (
                "approval_requested".into(),
                "access 7: port/web / web: serve; approve with shoal approvals --approve 7".into()
            ),
            ("agent_message".into(), "run shoal access approve 7".into()),
            (
                "question_asked".into(),
                "question 4 (answer with shoal questions 4): Which database?".into()
            ),
        ]
    );
    Ok(())
}

#[test]
fn migration_keeps_question_numbers_numbers_approvals_and_drops_delivered_questions() -> Result<()>
{
    let mut db = historical_database(45)?;
    db.execute_batch(
            r#"INSERT INTO agent_messages(id,workspace_id,created_at,message)
                VALUES (9,'workspace',1,'Answer to question 12 (Which cache?): Redis');
            INSERT INTO questions(id,workspace_id,created_at,question,recommendation,choices,answer,answered_at,message_id) VALUES
                (11,'workspace',1,'Which database?','SQLite','[]','SQLite',2,NULL),
                (12,'workspace',1,'Which cache?','Redis','[]','Redis',2,9),
                (13,'workspace',1,'Which queue?','SQS','[]',NULL,NULL,NULL);
            INSERT INTO access_requests(id,workspace_id,target_key,name,record,active) VALUES
                ('grant','workspace','port/web','web','{"id":"grant"}',0),
                ('land','workspace','land','branch','{"id":"land"}',1);"#,
        )?;
    migrate(&mut db)?;
    let questions = db
        .prepare("SELECT id,number FROM questions ORDER BY id")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(questions, [(12, 12), (13, 13)]);
    let requests = db
        .prepare(
            "SELECT id,number,json_extract(record,'$.id') FROM access_requests ORDER BY rowid",
        )?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(requests, [("grant".into(), 1, 1), ("land".into(), 2, 2)]);
    assert_eq!(free_number(&db, "questions")?, 1);
    assert_eq!(free_number(&db, "access_requests")?, 3);
    db.execute(
        "INSERT INTO questions(number,workspace_id,created_at,question,recommendation,choices)
            VALUES (1,'workspace',3,'Which logger?','tracing','[]')",
        [],
    )?;
    assert_eq!(free_number(&db, "questions")?, 2);
    Ok(())
}

#[test]
fn migration_retires_keep_requests_and_requests_the_cleanup_they_awaited() -> Result<()> {
    let mut db = historical_database(49)?;
    db.execute_batch(
        r#"INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                VALUES ('kept','repo','kept','/kept','kept','ready');
            INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES
                ('keep','workspace','keep','done','{"id":1}',1,1),
                ('land','workspace','land','branch','{"id":2}',1,2);
            INSERT INTO workspace_completion(workspace_id,record) VALUES
                ('workspace','{"head":"abc","cleanup":false,"error":null,"keep_request":1}'),
                ('kept','{"head":"def","cleanup":false,"error":null}');"#,
    )?;
    migrate(&mut db)?;
    let requests = db
        .prepare("SELECT id FROM access_requests")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(requests, ["land"]);
    let completions = db
        .prepare("SELECT workspace_id,record FROM workspace_completion ORDER BY workspace_id")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        completions,
        [
            (
                "kept".into(),
                r#"{"head":"def","cleanup":false,"error":null}"#.into()
            ),
            (
                "workspace".into(),
                r#"{"head":"abc","cleanup":true,"error":null}"#.into()
            ),
        ]
    );
    Ok(())
}

#[test]
fn migration_remembers_the_latest_branch_each_removal_record_names() -> Result<()> {
    let mut db = historical_database(52)?;
    db.execute_batch(
            r#"INSERT INTO repositories(id,path,source,last_used) VALUES ('kept','/kept','local',0);
            INSERT INTO removed_workspaces(workspace_id,repository_id,name,expires_at,record) VALUES
                ('old','kept','old',9,'{"removed_at":1,"checkpoint":{"branch":"topic","head":"aaa"}}'),
                ('new','kept','new',9,'{"removed_at":2,"checkpoint":{"branch":"topic","head":"bbb"}}'),
                ('detached','kept','detached',9,'{"removed_at":3,"checkpoint":{"branch":null,"head":"ccc"}}');"#,
        )?;
    migrate(&mut db)?;
    let kept = db
        .prepare("SELECT repository_id,branch,head,kept_at FROM kept_branches")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(kept, [("kept".into(), "topic".into(), "bbb".into(), 2)]);
    Ok(())
}

#[test]
fn failed_migration_rolls_back_all_pending_steps() -> Result<()> {
    let mut db = historical_database(1)?;
    db.execute_batch("ALTER TABLE executions ADD COLUMN wrapper TEXT;")?;
    let before = schema_snapshot(&db)?;
    let error = migrate(&mut db).unwrap_err();
    assert!(format!("{error:#}").contains("apply schema migration 10"));
    assert_eq!(schema_snapshot(&db)?, before);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
        1
    );
    db.execute_batch("ALTER TABLE executions DROP COLUMN wrapper;")?;
    migrate(&mut db)?;
    assert_eq!(
        executions(&db, "workspace")?[0].state,
        ExecutionState::Running
    );
    Ok(())
}

#[test]
fn newer_schema_is_rejected_without_changes() -> Result<()> {
    let mut db = historical_database(17)?;
    db.pragma_update(None, "user_version", SCHEMA_VERSION + 1)?;
    let before = schema_snapshot(&db)?;
    assert!(
        migrate(&mut db)
            .unwrap_err()
            .to_string()
            .contains("newer Shoal version")
    );
    assert_eq!(schema_snapshot(&db)?, before);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
        SCHEMA_VERSION + 1
    );
    Ok(())
}

#[test]
fn exists_checks_bound_queries_in_the_current_transaction() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    db.execute_batch("CREATE TABLE records(name TEXT);")?;
    let tx = db.transaction()?;
    tx.execute("INSERT INTO records(name) VALUES (?1)", ["present"])?;
    let sql = "SELECT 1 FROM records WHERE name=?1";
    assert!(exists(&tx, sql, ["present"])?);
    assert!(!exists(&tx, sql, ["missing"])?);
    assert!(exists(&tx, "SELECT 1 FROM missing_table", []).is_err());
    tx.rollback()?;
    assert!(!exists(&db, sql, ["present"])?);
    Ok(())
}

#[test]
fn mappers_use_named_columns() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute_batch(
        r#"INSERT INTO repositories(id,path,source,last_used,name,workspaces_dir)
                VALUES ('repo','/checkout','origin',42,'project','/workspaces');
            INSERT INTO workspaces(id,repository_id,name,path,branch,state,error,
                base_commit,base_ref,git_dir,git_dir_id,setup_finished)
                VALUES ('workspace','repo','worker','/work','feature','failed','setup error',
                    'abc','refs/heads/main','/git/worktrees/worker','1:2',1);
            INSERT INTO ports(workspace_id,name,port,env_var,reason)
                VALUES ('workspace','web',12345,'WEB_PORT',NULL);
            INSERT INTO executions(id,workspace_id,state,wrapper,child,group_id,agent)
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
        "workspaces",
        WORKSPACE_COLUMNS,
        workspace,
        serde_json::json!({
            "id": "workspace", "repository_id": "repo", "name": "worker", "path": "/work",
            "branch": "feature", "state": "failed", "error": "setup error",
            "base_commit": "abc", "base_ref": "refs/heads/main",
            "git_dir": "/git/worktrees/worker", "git_dir_id": "1:2",
            "base_workspace": null, "parent_workspace": null, "links": {"issue": null, "prs": [], "opened": [], "related": []}, "running": false, "done": false,
            "chat": null
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
            "id": "execution", "workspace_id": "workspace", "state": "unknown",
            "wrapper": {"pid": 123, "birth": "wrapper"},
            "child": {"pid": 456, "birth": "child"}, "group_id": 456,
            "agent": "claude"
        }),
    )?;
    assert!(setup_finished(&db, "workspace")?);
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

#[test]
fn execution_json_errors_report_the_named_column_index() -> Result<()> {
    let db = Connection::open_in_memory()?;
    let error = db
        .query_row(
            "SELECT 'bad json' AS child, NULL AS wrapper, 'e' AS id,
                'w' AS workspace_id, 'unknown' AS state, NULL AS group_id",
            [],
            execution,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, _)
    ));
    Ok(())
}

fn seed_interrupted_operations(db: &mut Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);",
    )?;
    for state in WorkspaceState::ALL {
        db.execute("INSERT INTO workspaces(id,repository_id,name,path,branch,state,error) VALUES (?1,'repo',?1,?1,?1,?1,'original error')", [state])?;
    }
    db.execute_batch("INSERT INTO executions(id,workspace_id,state) VALUES ('execution','preparing','running');
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

/// The operation snapshot of `seed_interrupted_operations` after quarantine.
fn quarantined_snapshot() -> Vec<String> {
    let mut rows: Vec<String> = WorkspaceState::ALL
        .iter()
        .map(|&original| {
            let (state, error) = match original {
                WorkspaceState::Preparing
                | WorkspaceState::Stopping
                | WorkspaceState::Removing
                | WorkspaceState::Reconciling
                | WorkspaceState::Restoring
                | WorkspaceState::Updating => (
                    WorkspaceState::Failed,
                    "daemon stopped during workspace operation; inspect before cleanup",
                ),
                WorkspaceState::Ready | WorkspaceState::Failed => (original, "original error"),
            };
            serde_json::json!([original, state, error]).to_string()
        })
        .collect();
    rows.push(serde_json::json!(["execution", ExecutionState::Unknown]).to_string());
    rows.push(r#"{"status":"interrupted","reason":"keep audit"}"#.into());
    rows.sort();
    rows
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

#[tokio::test]
async fn startup_quarantines_transient_workspaces_and_preserves_settled_states() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = Store::open(root.path().join("state.db")).await?;
    store.run(seed_interrupted_operations).await?;
    // A second quarantine finds nothing left to change.
    for _ in 0..2 {
        store.quarantine_interrupted_operations().await?;
        assert_eq!(store.run(operation_snapshot).await?, quarantined_snapshot());
    }
    Ok(())
}

#[tokio::test]
async fn failed_quarantine_rolls_back_without_undoing_migration() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await.unwrap();
    store.run(seed_interrupted_operations).await.unwrap();
    store
        .run(|db| {
            event_tests::remove_schema(db)?;
            db.execute_batch(
                "DROP TABLE access_requests;
                PRAGMA user_version=16;
                CREATE TRIGGER fail_quarantine BEFORE UPDATE OF state ON workspaces
                    BEGIN SELECT RAISE(ABORT, 'quarantine blocked'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let before = store.run(operation_snapshot).await.unwrap();
    let migrated = Store::open(path.clone()).await.unwrap();
    let error = migrated
        .quarantine_interrupted_operations()
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("quarantine blocked"));
    assert_eq!(migrated.run(operation_snapshot).await.unwrap(), before);
    migrated.run(|db| {
            assert_eq!(db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?, SCHEMA_VERSION);
            db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('request','preparing','pool','default','{}',1,1)", [])?;
            db.execute_batch("DROP TRIGGER fail_quarantine;")?;
            Ok(())
        }).await.unwrap();
    migrated.quarantine_interrupted_operations().await.unwrap();
    let recovered = migrated.run(operation_snapshot).await.unwrap();
    assert_eq!(recovered, quarantined_snapshot());
    let reopened = Store::open(path).await.unwrap();
    reopened.quarantine_interrupted_operations().await.unwrap();
    assert_eq!(reopened.run(operation_snapshot).await.unwrap(), recovered);
}

#[tokio::test]
async fn approval_records_survive_reopen_and_follow_workspace_ownership() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await.unwrap();
    store.run(|db| {
            event_tests::remove_schema(db)?;
            db.execute_batch("DROP TABLE access_requests;
                PRAGMA user_version=16;
                INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES ('workspace','repo','worker','/work','worker','ready');")?;
            Ok(())
        }).await.unwrap();
    let migrated = Store::open(path.clone()).await.unwrap();
    migrated.run(|db| {
            db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('request','workspace','pool','default','{}',1,1)", [])?;
            assert!(db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('duplicate','workspace','pool','default','{}',1,2)", []).is_err());
            assert!(db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('orphan','missing','pool','default','{}',1,3)", []).is_err());
            assert!(db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('renumbered','workspace','pool','other','{}',1,1)", []).is_err());
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
            db.execute("DELETE FROM workspaces", [])?;
            assert_eq!(count(db)?, 0);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn migrates_original_database_without_losing_ownership_records() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE repositories(id TEXT PRIMARY KEY,path TEXT UNIQUE,source TEXT,last_used INTEGER);
            CREATE TABLE workspaces(id TEXT PRIMARY KEY,repository_id TEXT,name TEXT UNIQUE,path TEXT UNIQUE,branch TEXT,state TEXT,error TEXT);
            CREATE TABLE executions(id TEXT PRIMARY KEY,workspace_id TEXT,state TEXT);
            INSERT INTO repositories VALUES ('repo','/repo','/repo',1);
            INSERT INTO workspaces VALUES ('workspace','repo','feature','/work','feature','ready',NULL);
            INSERT INTO executions VALUES ('execution','workspace','running');
            PRAGMA user_version=1;").unwrap();
    drop(db);
    let store = Store::open(path.clone()).await.unwrap();
    store
        .run(|db| {
            let repo = db.query_row(
                &format!("SELECT {REPOSITORY_COLUMNS} FROM repositories"),
                [],
                repository,
            )?;
            assert_eq!(repo.id, "repo");
            assert!(repo.name.is_none() && repo.workspaces_dir.is_none());
            let workspace = db.query_row(
                &format!("SELECT {WORKSPACE_COLUMNS} FROM workspaces"),
                [],
                workspace,
            )?;
            assert_eq!(workspace.name, "feature");
            assert!(workspace.base_commit.is_none() && workspace.base_ref.is_none());
            assert!(setup_finished(db, "workspace")?);
            assert_eq!(
                executions(db, "workspace")?[0].state,
                crate::state::ExecutionState::Running
            );
            assert!(ports(db, None)?.is_empty());
            Ok(())
        })
        .await
        .unwrap();
    store.quarantine_interrupted_operations().await.unwrap();
    Store::open(path)
        .await
        .unwrap()
        .run(|db| {
            assert_eq!(
                executions(db, "workspace")?[0].state,
                crate::state::ExecutionState::Unknown
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn migrates_semaphore_leases_and_definitions_from_version_eight() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await.unwrap();
    store.run(|db| {
            event_tests::remove_schema(db)?;
            db.execute_batch("INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES ('workspace','repo','worker','/work','worker','ready');
                ALTER TABLE resource_leases DROP COLUMN mode;
                ALTER TABLE workspaces DROP COLUMN git_dir;
                ALTER TABLE workspaces DROP COLUMN git_dir_id;
                ALTER TABLE workspaces DROP COLUMN setup_finished;
                ALTER TABLE executions DROP COLUMN wrapper;
                ALTER TABLE executions DROP COLUMN child;
                ALTER TABLE executions DROP COLUMN group_id;
                ALTER TABLE repositories DROP COLUMN workspaces_dir;
                PRAGMA user_version=8;")?;
            let definition = r#"{"capacity":2,"reason":null,"resources":{"worker":{"capacity":2,"reason":null}}}"#;
            db.execute("INSERT INTO resource_pools(scope,name,definition) VALUES ('global','worker',?1)", [definition])?;
            db.execute("INSERT INTO resource_leases VALUES ('lease','workspace','global','worker','default','worker',NULL,123)", [])?;
            Ok(())
        }).await.unwrap();
    let migrated = Store::open(path.clone()).await.unwrap();
    migrated
            .run(|db| {
                let leases = crate::daemon::resources::leases(db, None)?;
                assert_eq!(leases.len(), 1);
                assert_eq!(leases[0].id, "lease");
                assert_eq!(leases[0].mode, crate::daemon::resources::LockMode::Permit);
                assert_eq!(leases[0].created_at, 123);
                let json: String =
                    db.query_row("SELECT definition FROM resource_pools", [], |row| {
                        row.get(0)
                    })?;
                let stored: crate::daemon::resources::Definition = serde_json::from_str(&json)?;
                let config: crate::config::repo::RepoConfig =
                    toml::from_str("[resources.worker]\ncapacity=2")?;
                assert_eq!(
                    stored,
                    crate::daemon::resources::definitions(
                        &config.resources,
                        &config.resource_pools
                    )?["worker"]
                );
                assert!(
                    db.execute("UPDATE resource_leases SET mode='invalid'", [])
                        .is_err()
                );
                assert!(
                    db.query_row("SELECT 'invalid'", [], |row| {
                        row.get::<_, crate::daemon::resources::LockMode>(0)
                    })
                    .is_err()
                );
                Ok(())
            })
            .await
            .unwrap();
    Store::open(path).await.unwrap();
}

#[tokio::test]
async fn migrates_setup_completion_without_losing_interrupted_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await.unwrap();
    store
            .run(|db| {
                event_tests::remove_schema(db)?;
                db.execute_batch(
                    "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                    INSERT INTO workspaces(id,repository_id,name,path,branch,state,error) VALUES
                        ('ready','repo','ready','/ready','ready','ready',NULL),
                        ('preparing','repo','preparing','/preparing','preparing','preparing',NULL),
                        ('failed','repo','failed','/failed','failed','failed','setup failed (exit 1, processes stopped: true); retry with shoal prepare');
                    ALTER TABLE workspaces DROP COLUMN setup_finished;
                    PRAGMA user_version=15;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
    let migrated = Store::open(path).await.unwrap();
    migrated
        .run(|db| {
            assert!(setup_finished(db, "ready")?);
            assert!(!setup_finished(db, "preparing")?);
            assert!(!setup_finished(db, "failed")?);
            let state: WorkspaceState = db.query_row(
                "SELECT state FROM workspaces WHERE id='preparing'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(state, WorkspaceState::Preparing);
            Ok(())
        })
        .await
        .unwrap();
    migrated.quarantine_interrupted_operations().await.unwrap();
    migrated
        .run(|db| {
            assert!(!setup_finished(db, "preparing")?);
            assert_eq!(
                db.query_row(
                    "SELECT state FROM workspaces WHERE id='preparing'",
                    [],
                    |row| row.get::<_, WorkspaceState>(0)
                )?,
                WorkspaceState::Failed
            );
            Ok(())
        })
        .await
        .unwrap();
}
