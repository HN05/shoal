//! Every trigger that records events, each fired once: what it records,
//! and that each record decodes and encodes back to itself.
use super::*;
use anyhow::Context as _;
use serde_json::json;

/// A trigger's case: the rows it starts from, the change that fires it,
/// and part of each event the change records, in order.
struct Case {
    trigger: &'static str,
    setup: &'static str,
    fire: &'static str,
    recorded: Vec<Value>,
}

/// The fixture: the workspaces `base` and `work`, which builds on `base`
/// and is below the chat `chat`.
fn fixture() -> Result<Connection> {
    let db = migrated()?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         INSERT INTO chats(id) VALUES ('chat');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('chat','chat',NULL,'planner');
         INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
             VALUES ('base','/base','api','ready','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('base','workspace','repo','api');
         INSERT INTO space_parents(space_id,parent_id) VALUES ('work','chat');
         INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir,base_workspace_id)
             VALUES ('work','/work','fix','ready','abc','/admin','base');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('work','workspace','repo','fix');
         COMMIT;",
    )?;
    Ok(db)
}

const THREAD: &str =
    "INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener)
    VALUES (1,'work',3,1,'question','Which database?',1,'space');";

const HELD_THREAD: &str =
    "INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener,held)
    VALUES (1,'work',3,1,'question','Which database?',1,'space',1);
    INSERT INTO thread_posts(thread_id,created_at,side,sender,sender_name,text)
        VALUES (1,1,'opener','space','fix','Postgres?');";

const FILE: &str = "INSERT INTO space_files(id,space_id,space,repository_id,source,name,path,bytes,
        created_at,attached_by)
    VALUES (7,'work','fix','repo','added','notes.md','/files/notes.md',3,1,'{\"kind\":\"user\"}');";

const LEASED: &str = r#"INSERT INTO simulators(id,record) VALUES ('sim','{"udid":"U","device":"iPhone",
    "runtime":"iOS 18","workspace_id":"work","lease_name":"default","state":"leased"}');"#;

const DEMO: &str = r#"INSERT INTO executions(id,space_id,state) VALUES ('e','work','running');
    INSERT INTO demos(execution_id,workspace_id,name,started_at,record,has_run)
    VALUES ('e','work','app',1,'{"preset":"simulator","environment":{"SHOAL_SIM_UDID":"U"}}',0);"#;

const RUN_DEMO: &str = r#"INSERT INTO executions(id,space_id,state) VALUES ('e','work','running');
    INSERT INTO demos(execution_id,workspace_id,name,started_at,record,has_run)
    VALUES ('e','work','app',1,'{"preset":"simulator","environment":{"SHOAL_SIM_UDID":"U"}}',1);"#;

fn cases() -> Vec<Case> {
    let thread = json!({"space_number": 3, "item": 1, "number": "3.1", "name": "Which database?"});
    let with = |extra: Value| {
        let mut thread = thread.clone();
        thread
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        thread
    };
    let file = json!({"id": 7, "space": {"id": "work", "name": "fix", "kind": "workspace"},
        "source": "added", "name": "notes.md", "path": "/files/notes.md", "bytes": 3,
        "created_at": 1, "attached_by": {"kind": "user"}});
    let simulator =
        json!({"name": "default", "udid": "U", "device": "iPhone", "runtime": "iOS 18"});
    let demo = json!({"name": "app", "preset": "simulator", "udid": "U"});
    let review = json!({"kind": "pr", "url": "https://forge.example/pulls/1"});
    let case = |trigger, setup, fire, recorded| Case {
        trigger,
        setup,
        fire,
        recorded,
    };
    vec![
        case(
            "space_renamed",
            "",
            "UPDATE spaces SET name='retry' WHERE id='work'",
            vec![json!({"space_id": "work", "kind": "renamed", "name": "retry"})],
        ),
        case(
            "space_converted",
            "",
            "UPDATE spaces SET kind='chat' WHERE id='work'",
            vec![
                json!({"space_id": "work", "kind": "converted", "space_kind": "chat",
                "handoff": null}),
            ],
        ),
        case(
            "space_leaving",
            "",
            "DELETE FROM spaces WHERE id='work'",
            vec![json!({"space_id": "chat", "kind": "child_removed",
                "child": {"id": "work", "name": "fix", "kind": "workspace", "branch": "fix"}})],
        ),
        case(
            "space_parent_set",
            "",
            "INSERT INTO space_parents(space_id,parent_id) VALUES ('base','chat')",
            vec![json!({"space_id": "base", "kind": "parent_changed",
                "parent": {"id": "chat", "name": "planner", "kind": "chat"}})],
        ),
        case(
            "space_parent_changed",
            "",
            "UPDATE space_parents SET parent_id='base' WHERE space_id='work'",
            vec![json!({"space_id": "work", "kind": "parent_changed",
                "parent": {"id": "base", "name": "api", "kind": "workspace", "branch": "api"}})],
        ),
        case(
            "space_parent_cleared",
            "",
            "DELETE FROM space_parents WHERE space_id='work'",
            vec![json!({"space_id": "work", "kind": "parent_changed", "parent": null})],
        ),
        case(
            "space_review_ready",
            "",
            "INSERT INTO space_review(space_id,url,kind)
                VALUES ('work','https://forge.example/pulls/1','pr')",
            vec![json!({"space_id": "work", "kind": "review_ready", "review": review})],
        ),
        case(
            "space_review_ready_again",
            "INSERT INTO space_review(space_id,url,kind) VALUES ('work','',NULL)",
            "UPDATE space_review SET created_at=2 WHERE space_id='work'",
            vec![json!({"space_id": "work", "kind": "review_ready",
                "review": {"kind": null, "url": null}})],
        ),
        case(
            "space_review_cleared",
            "INSERT INTO space_review(space_id,url,kind)
                VALUES ('work','https://forge.example/pulls/1','pr')",
            "DELETE FROM space_review WHERE space_id='work'",
            vec![json!({"space_id": "work", "kind": "review_cleared", "review": review})],
        ),
        case(
            "space_agent_state_set",
            "",
            "INSERT INTO space_agent_state(space_id,state,since) VALUES ('work','working',1)",
            vec![json!({"space_id": "work", "kind": "agent_state",
                "agent_state": {"state": "working"}})],
        ),
        case(
            "space_agent_state_changed",
            "INSERT INTO space_agent_state(space_id,state,since) VALUES ('work','working',1)",
            "UPDATE space_agent_state SET state='idle' WHERE space_id='work'",
            vec![json!({"space_id": "work", "kind": "agent_state",
                "agent_state": {"state": "idle"}})],
        ),
        case(
            "space_agent_state_cleared",
            "INSERT INTO space_agent_state(space_id,state,since) VALUES ('work','working',1)",
            "DELETE FROM space_agent_state WHERE space_id='work'",
            vec![json!({"space_id": "work", "kind": "agent_state",
                "agent_state": {"state": null}})],
        ),
        case(
            "space_completed",
            "",
            "INSERT INTO space_completion(space_id,record,cause)
                VALUES ('work','{\"error\":null}','idle')",
            vec![json!({"space_id": "work", "kind": "completed", "cause": "idle"})],
        ),
        case(
            "space_completed_again",
            "INSERT INTO space_completion(space_id,record) VALUES ('work','{\"error\":\"x\"}')",
            "UPDATE space_completion SET record='{\"error\":null}' WHERE space_id='work'",
            vec![json!({"space_id": "work", "kind": "completed", "cause": "manual"})],
        ),
        case(
            "space_undone",
            "INSERT INTO space_completion(space_id,record) VALUES ('work','{\"error\":null}')",
            "DELETE FROM space_completion WHERE space_id='work'",
            vec![json!({"space_id": "work", "kind": "undone"})],
        ),
        case(
            "demo_started",
            DEMO,
            "UPDATE demos SET has_run=1",
            vec![json!({"space_id": "work", "kind": "demo_started", "demo": demo})],
        ),
        case(
            "demo_stopped",
            RUN_DEMO,
            // The execution's end deletes its demo.
            "DELETE FROM executions",
            vec![json!({"space_id": "work", "kind": "demo_stopped", "demo": demo})],
        ),
        case(
            "simulator_acquired",
            r#"INSERT INTO simulators(id,record) VALUES ('sim','{"udid":"U","device":"iPhone",
                "runtime":"iOS 18","workspace_id":null,"lease_name":null,"state":"idle"}')"#,
            r#"UPDATE simulators SET record='{"udid":"U","device":"iPhone","runtime":"iOS 18",
                "workspace_id":"work","lease_name":"default","state":"leased"}'"#,
            vec![json!({"space_id": "work", "kind": "simulator_acquired",
                "simulator": simulator})],
        ),
        case(
            "simulator_released",
            LEASED,
            r#"UPDATE simulators SET record='{"udid":"U","device":"iPhone","runtime":"iOS 18",
                "workspace_id":null,"lease_name":null,"state":"idle"}'"#,
            vec![json!({"space_id": "work", "kind": "simulator_released",
                "simulator": simulator})],
        ),
        case(
            "simulator_deleted",
            LEASED,
            "DELETE FROM simulators",
            vec![json!({"space_id": "work", "kind": "simulator_released",
                "simulator": simulator})],
        ),
        case(
            "space_created",
            "",
            "BEGIN;
             INSERT INTO space_parents(space_id,parent_id) VALUES ('next','work');
             INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir,base_workspace_id,
                 base_ref)
                 VALUES ('next','/next','next','ready','abc','/admin','work','refs/heads/fix');
             INSERT INTO spaces(id,kind,repository_id,name)
                 VALUES ('next','workspace','repo','next');
             COMMIT;",
            vec![
                json!({"space_id": "next", "kind": "created", "base_ref": "refs/heads/fix",
                    "base_workspace": {"id": "work", "name": "fix", "branch": "fix"},
                    "parent": {"id": "work", "name": "fix", "kind": "workspace",
                        "branch": "fix"}}),
                json!({"space_id": "next", "kind": "ready"}),
                json!({"space_id": "work", "kind": "child_created",
                    "child": {"id": "next", "name": "next", "kind": "workspace",
                        "branch": "next"}}),
            ],
        ),
        case(
            "workspace_base_recorded",
            "",
            "UPDATE workspaces SET base_ref='refs/heads/main' WHERE id='work'",
            vec![],
        ),
        case(
            "workspace_base_removed",
            "",
            "DELETE FROM workspaces WHERE id='base'",
            vec![
                json!({"space_id": "work", "kind": "base_changed", "cause": "removed",
                "base_workspace": null}),
            ],
        ),
        case(
            "workspace_branch_changed",
            "",
            "UPDATE workspaces SET observed_branch='other' WHERE id='work'",
            vec![json!({"space_id": "work", "kind": "branch_changed", "branch": "other"})],
        ),
        case(
            "workspace_branch_renamed",
            "",
            "UPDATE workspaces SET branch='fix-2' WHERE id='work'",
            vec![json!({"space_id": "work", "kind": "renamed", "branch": "fix-2"})],
        ),
        case(
            "workspace_ready",
            "UPDATE workspaces SET state='preparing' WHERE id='work'",
            "UPDATE workspaces SET state='ready' WHERE id='work'",
            vec![json!({"space_id": "work", "kind": "ready"})],
        ),
        case(
            "workspace_setup_failed",
            "UPDATE workspaces SET state='preparing' WHERE id='work'",
            "UPDATE workspaces SET state='failed',error='setup exited 1' WHERE id='work'",
            vec![json!({"space_id": "work", "kind": "setup_failed",
                "error": "setup exited 1"})],
        ),
        case(
            "thread_opened",
            "",
            THREAD,
            vec![json!({"space_id": "work", "kind": "thread_opened", "thread": thread})],
        ),
        case(
            "thread_released",
            HELD_THREAD,
            "UPDATE threads SET held=0",
            vec![
                json!({"space_id": "work", "kind": "thread_opened", "thread": thread}),
                json!({"space_id": "work", "kind": "thread_posted",
                    "thread": with(json!({"text": "Postgres?", "sender": "fix"}))}),
            ],
        ),
        case(
            "thread_posted",
            THREAD,
            "INSERT INTO thread_posts(thread_id,created_at,side,sender,text)
                VALUES (1,2,'holder','user','SQLite')",
            vec![json!({"space_id": "work", "kind": "thread_posted",
                "thread": with(json!({"text": "SQLite", "sender": "the user"}))})],
        ),
        case(
            "thread_forwarded",
            THREAD,
            "INSERT INTO thread_posts(thread_id,created_at,side,sender,text,forwards,forwarded_to)
                VALUES (1,2,'holder','shoal','Over to you',1,'chat')",
            vec![json!({"space_id": "work", "kind": "thread_forwarded",
                "thread": with(json!({"text": "Over to you", "sender": "Shoal",
                    "to": "planner"}))})],
        ),
        case(
            "thread_resolved",
            THREAD,
            "UPDATE threads SET outcome='answered',resolved_by='space',
                resolved_at=3",
            vec![json!({"space_id": "work", "kind": "thread_resolved",
                "thread": with(json!({"outcome": "answered"}))})],
        ),
        case(
            "thread_resolved",
            "INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener,
                    outcome,outcome_details,resolved_by,resolved_at)
                VALUES (1,'work',3,1,'review','Which database?',1,'shoal','deferred','{\"issue\":\"#1\"}',
                    'space',2);",
            "UPDATE threads SET outcome_details='{\"issue\":\"#2\"}',resolved_at=3",
            vec![json!({"space_id": "work", "kind": "thread_resolved",
                "thread": with(json!({"outcome": "deferred"}))})],
        ),
        case(
            "agent_handoff_started",
            r#"INSERT INTO agent_handoffs(space_id,agent,record) VALUES ('work','codex',
                '{"from":{"agent":"claude","session":"before","integration":"claude"},
                  "to":{"agent":"codex","integration":"codex"}}')"#,
            "UPDATE agent_handoffs SET session='after'",
            vec![json!({"space_id": "work", "kind": "converted", "handoff": {
                "from": {"agent": "claude", "session": "before", "integration": "claude"},
                "to": {"agent": "codex", "session": "after", "integration": "codex"}}})],
        ),
        case(
            "file_added",
            "",
            FILE,
            vec![
                json!({"space_id": "work", "kind": "file_added", "repository_id": "repo",
                "file": file}),
            ],
        ),
        case(
            "file_removed",
            FILE,
            "DELETE FROM space_files",
            vec![
                json!({"space_id": "work", "kind": "file_removed", "repository_id": "repo",
                "file": file}),
            ],
        ),
    ]
}

/// The journal's records after `after`, each checked to decode and
/// encode back to itself.
fn records_after(db: &Connection, after: i64) -> Result<Vec<Value>> {
    let mut statement = db.prepare("SELECT record FROM space_events WHERE id>?1 ORDER BY id")?;
    let rows = statement.query_map([after], |row| row.get::<_, String>(0))?;
    rows.map(|text| {
        let stored: Value = serde_json::from_str(&text?)?;
        let encoded = match stored["kind"].as_str() {
            Some("file_added" | "file_removed") => {
                serde_json::to_value(serde_json::from_value::<
                    shoal_protocol::events::FileEventDetails,
                >(stored.clone())?)?
            }
            _ => serde_json::to_value(serde_json::from_value::<EventDetails>(stored.clone())?)?,
        };
        assert_eq!(encoded, stored);
        Ok(stored)
    })
    .collect()
}

/// Whether `record` holds each field of `part` with its value.
fn holds(record: &Value, part: &Value) -> bool {
    (part.as_object().unwrap().iter()).all(|(field, value)| &record[field] == value)
}

fn newest_id(db: &Connection) -> Result<i64> {
    Ok(
        db.query_row("SELECT COALESCE(MAX(id),0) FROM space_events", [], |row| {
            row.get(0)
        })?,
    )
}

/// The triggers whose SQL records events.
fn recording_triggers(db: &Connection) -> Result<Vec<String>> {
    let mut statement = db.prepare(
        "SELECT name FROM sqlite_schema WHERE type='trigger'
            AND (sql LIKE '%journal_append(%' OR sql LIKE '%journal_amend_created_base(%')
            ORDER BY name",
    )?;
    let rows = statement.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[test]
fn each_trigger_that_records_events_has_a_case() -> Result<()> {
    let mut covered: Vec<_> = cases().iter().map(|case| case.trigger.to_owned()).collect();
    covered.sort();
    // A trigger may have several cases, such as for each change it records.
    covered.dedup();
    assert_eq!(covered, recording_triggers(&fixture()?)?);
    Ok(())
}

#[test]
fn each_trigger_records_its_events_as_shoal_s_types_encode_them() -> Result<()> {
    for case in cases() {
        let db = fixture()?;
        records_after(&db, 0)?;
        db.execute_batch(case.setup)
            .with_context(|| format!("set up {}", case.trigger))?;
        let before = newest_id(&db)?;
        db.execute_batch(case.fire)
            .with_context(|| format!("fire {}", case.trigger))?;
        let recorded = records_after(&db, before)?;
        assert_eq!(
            recorded.len(),
            case.recorded.len(),
            "{}: {recorded:#?}",
            case.trigger
        );
        for (record, part) in recorded.iter().zip(&case.recorded) {
            assert!(
                holds(record, part),
                "{}: {record:#} lacks {part:#}",
                case.trigger
            );
        }
    }
    Ok(())
}

#[test]
fn a_kind_the_journal_does_not_know_fails_the_statement() -> Result<()> {
    let db = fixture()?;
    let call = |sql: &str| db.query_row(sql, [], |_| Ok(()));
    let unknown = call("SELECT journal_append('deleted','work',json_object())");
    assert!(format!("{:#}", unknown.unwrap_err()).contains("unknown event kind deleted"));
    let unexpected = call("SELECT journal_append('renamed','work',json_object('name','retry'))");
    assert!(format!("{:#}", unexpected.unwrap_err()).contains("invalid trigger source"));
    Ok(())
}
