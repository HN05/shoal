use super::*;
use rusqlite::types::Value;

/// The variable that has [`a_new_database_takes_the_schema_the_migrations_make`]
/// write [`SCHEMA`] anew from the migrations.
const WRITE_SCHEMA: &str = "SHOAL_WRITE_SCHEMA";

/// The schema [`FLOOR_RELEASE`] wrote, which every migration starts from.
const FLOOR_SCHEMA: &str = include_str!("../../tests/fixtures/schema_v0.21.0.sql");

/// A database of [`FLOOR_RELEASE`], at [`FLOOR`].
fn at_floor() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.execute_batch(FLOOR_SCHEMA)?;
    db.pragma_update(None, "user_version", FLOOR)?;
    Ok(db)
}

/// A database of [`FLOOR_RELEASE`] that has taken every migration.
fn migrated() -> Result<Connection> {
    let mut db = at_floor()?;
    migrate(&mut db)?;
    Ok(db)
}

/// The statements that made `db`'s schema objects of `kind`, in the order
/// it made them.
fn statements(db: &Connection, kind: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare(
        "SELECT sql FROM sqlite_schema WHERE type=?1 AND sql IS NOT NULL
            AND name NOT LIKE 'sqlite_%' ORDER BY rowid",
    )?;
    let rows = statement.query_map([kind], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The names of `db`'s tables, SQLite's own included.
fn tables(db: &Connection) -> Result<Vec<String>> {
    let mut statement =
        db.prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY rowid")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The rows of `table`, each as the SQL values that insert it.
fn rows(db: &Connection, table: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare(&format!("SELECT * FROM \"{table}\""))?;
    let columns = statement.column_count();
    let rows = statement.query_map([], |row| {
        let values = (0..columns)
            .map(|column| row.get::<_, Value>(column).map(literal))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(format!("({})", values.join(", ")))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `value` as an SQL literal.
fn literal(value: Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Integer(number) => number.to_string(),
        Value::Real(number) => number.to_string(),
        Value::Text(text) => format!("'{}'", text.replace('\'', "''")),
        Value::Blob(bytes) => {
            let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("X'{hex}'")
        }
    }
}

/// The SQL that makes `db`'s schema with the rows it holds: tables first,
/// so that each index finds its table, then the rows, before the triggers
/// that inserting them would set off.
fn schema_of(db: &Connection) -> Result<String> {
    let mut sql = statements(db, "table")?;
    sql.extend(statements(db, "index")?);
    sql.extend(statements(db, "view")?);
    for table in tables(db)? {
        let inserts = rows(db, &table)?.into_iter();
        sql.extend(inserts.map(|row| format!("INSERT INTO \"{table}\" VALUES {row}")));
    }
    sql.extend(statements(db, "trigger")?);
    Ok(format!(
        "-- The schema the migrations in migrations.rs make, which a new database\n\
         -- takes. Written by\n\
         -- {WRITE_SCHEMA}=1 cargo nextest run -p shoal-store -E 'test(a_new_database_takes)'\n\n{};\n",
        sql.join(";\n\n")
    ))
}

/// Each schema object of `db`, SQLite's own included, with the statement
/// that made it and a table's rows, and the schema version.
type Contents = (Vec<(String, Option<String>, Vec<String>)>, i64);

fn contents(db: &Connection) -> Result<Contents> {
    let mut statement = db.prepare(
        "SELECT type || ' ' || name, sql, type='table', name FROM sqlite_schema
            ORDER BY type, name",
    )?;
    let objects = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<(String, Option<String>, bool, String)>>>()?;
    let mut found = Vec::new();
    for (object, sql, table, name) in objects {
        let rows = if table { rows(db, &name)? } else { Vec::new() };
        found.push((object, sql, rows));
    }
    let version = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok((found, version))
}

#[test]
fn a_new_database_takes_the_schema_the_migrations_make() -> Result<()> {
    let migrated = migrated()?;
    if std::env::var_os(WRITE_SCHEMA).is_some() {
        // Read at run time: a test binary a workspace keeps from its seed
        // build was compiled in the seed's checkout.
        let package = std::env::var_os("CARGO_MANIFEST_DIR").context("no CARGO_MANIFEST_DIR")?;
        let path = std::path::Path::new(&package).join("src/migrations/schema.sql");
        std::fs::write(path, schema_of(&migrated)?)?;
    }
    let mut created = Connection::open_in_memory()?;
    created.pragma_update(None, "foreign_keys", true)?;
    migrate(&mut created)?;
    assert!(
        contents(&created)? == contents(&migrated)?,
        "schema.sql is not what the migrations make; write it anew with \
         {WRITE_SCHEMA}=1 cargo nextest run -p shoal-store -E 'test(a_new_database_takes)'"
    );
    Ok(())
}

#[test]
fn a_database_older_than_the_floor_is_refused_without_changes() -> Result<()> {
    let mut older = at_floor()?;
    older.pragma_update(None, "user_version", FLOOR - 1)?;
    let mut unversioned = at_floor()?;
    unversioned.pragma_update(None, "user_version", 0)?;
    for db in [&mut older, &mut unversioned] {
        let before = contents(db)?;
        let error = migrate(db).unwrap_err();
        assert!(format!("{error:#}").contains(FLOOR_RELEASE), "{error:#}");
        assert!(contents(db)? == before);
    }
    Ok(())
}

#[test]
fn a_database_newer_than_this_version_is_refused_without_changes() -> Result<()> {
    let mut db = migrated()?;
    db.pragma_update(None, "user_version", SCHEMA_VERSION + 1)?;
    let before = contents(&db)?;
    assert!(migrate(&mut db).is_err());
    assert!(contents(&db)? == before);
    Ok(())
}

#[test]
fn a_failed_migration_rolls_back_every_step_of_its_batch() -> Result<()> {
    let mut db = at_floor()?;
    // A migration creates this table.
    db.execute_batch("CREATE TABLE run_usage (id INTEGER PRIMARY KEY);")?;
    let before = contents(&db)?;
    assert!(migrate(&mut db).is_err());
    assert!(contents(&db)? == before);
    Ok(())
}

#[test]
fn reviews_keep_their_ids_and_reopens_once_they_can_fail_unsubmitted() -> Result<()> {
    let mut db = at_floor()?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        BEGIN;
        INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
            VALUES ('workspace','/work','worker','ready','abc','/admin');
        INSERT INTO spaces(id,kind,repository_id,name)
            VALUES ('workspace','workspace','repo','worker');
        COMMIT;
        INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener,holder_id)
            VALUES (1,'workspace',1,1,'review','Still',1,'shoal','workspace');
        INSERT INTO reviews(id,workspace_id,target,opener,agent,state,created_at)
            VALUES (1,'workspace','{\"kind\":\"workspace\"}','shoal','codex','finished',1),
                (2,'workspace','{\"kind\":\"workspace\"}','shoal','codex','cancelled',2);
        INSERT INTO review_reopens(review_id,thread_id) VALUES (1,1);
        DELETE FROM reviews WHERE id=2;",
    )?;
    migrate(&mut db)?;
    let reopened: (i64, i64) = db.query_row(
        "SELECT review_id,thread_id FROM review_reopens",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(reopened, (1, 1));
    db.execute(
        "INSERT INTO reviews(workspace_id,target,opener,agent,state,failure,created_at)
            VALUES ('workspace','{\"kind\":\"workspace\"}','shoal','codex','failed','not-submitted',3)",
        [],
    )?;
    assert_eq!(db.last_insert_rowid(), 3);
    Ok(())
}

#[test]
fn ci_job_runs_move_to_the_runs_as_whole_runs_of_their_job() -> Result<()> {
    let mut db = at_floor()?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        INSERT INTO ci_job_usage(repository_id,workflow,name,ended_at,peak_memory)
            VALUES ('repo','ci.yml','rust (fmt, test)',5,900);",
    )?;
    migrate(&mut db)?;
    let run: (String, String, i64, i64, Option<i64>, bool, bool) = db.query_row(
        "SELECT kind,key,ended_at,peak_memory,duration,whole,oom_killed FROM run_usage",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    let key = r#"["ci.yml","rust (fmt, test)"]"#.to_owned();
    assert_eq!(run, ("ci_job".into(), key, 5, 900, None, true, false));
    Ok(())
}

mod agent_modes;
mod alert_attachments;
mod approval_columns;
mod checkpoint_columns;
mod clean_request_columns;
mod completion_error;
mod conversion_columns;
mod github_activity;
mod hub_numbers;
mod item_activity_states;
mod journal_columns;
mod journal_shapes;
mod pending_plan_columns;
mod post_files;
mod removed_space_columns;
mod review_targets;
mod schema;
mod simulator_columns;
mod space_blockers;
mod thread_outcomes;

/// A thread at the floor where the holder offered choices twice, the user
/// picked the second of the first offer, a post on the holder's own side
/// picked one too, and the user resolved it with the first of the second
/// offer.
fn thread_with_picks(db: &Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        INSERT INTO spaces(id,kind,repository_id,name) VALUES ('chat','chat','repo','asker');
        INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener,
                holder_id,outcome,resolved_by,resolved_at)
            VALUES (1,'chat',1,1,'question','Which database?',1,'user','chat',
                '{\"outcome\":\"answered\",\"choice\":1}','user',9);
        INSERT INTO thread_posts(id,thread_id,created_at,side,sender,sender_id,text,choices,
                recommendation,reason)
            VALUES (1,1,2,'holder','space','chat','Which?','[\"Postgres\",\"SQLite\"]',
                'SQLite','Fewer moving parts');
        INSERT INTO thread_posts(id,thread_id,created_at,side,sender,text,choice)
            VALUES (2,1,3,'opener','user','SQLite',2);
        INSERT INTO thread_posts(id,thread_id,created_at,side,sender,sender_id,text,choice)
            VALUES (3,1,4,'holder','space','chat','Mine',1);
        INSERT INTO thread_posts(id,thread_id,created_at,side,sender,sender_id,text,choices)
            VALUES (4,1,5,'holder','space','chat','And the cache?','[\"Redis\",\"None\"]');",
    )?;
    Ok(())
}

/// The `ui` and `answer` of the post `id`, as JSON.
fn interaction(
    db: &Connection,
    id: i64,
) -> Result<(Option<serde_json::Value>, Option<serde_json::Value>)> {
    let (ui, answer): (Option<String>, Option<String>) = db.query_row(
        "SELECT ui,answer FROM thread_posts WHERE id=?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let parse = |text: Option<String>| text.map(|text| serde_json::from_str(&text)).transpose();
    Ok((parse(ui)?, parse(answer)?))
}

#[test]
fn choices_become_the_surfaces_the_shorthand_builds() -> Result<()> {
    let mut db = at_floor()?;
    thread_with_picks(&db)?;
    migrate(&mut db)?;
    let choices = ["Postgres".to_owned(), "SQLite".to_owned()];
    let built = shoal_protocol::surfaces::Surface::choices(
        &choices,
        Some("SQLite"),
        Some("Fewer moving parts"),
    );
    assert_eq!(interaction(&db, 1)?.0.as_ref(), Some(built.as_json()));
    Ok(())
}

#[test]
fn a_recommendation_without_choices_becomes_a_surface_too() -> Result<()> {
    let mut db = at_floor()?;
    thread_with_picks(&db)?;
    db.execute(
        "INSERT INTO thread_posts(id,thread_id,created_at,side,sender,sender_id,text,
                recommendation,reason)
            VALUES (5,1,6,'holder','space','chat','Port?','8080','Free here')",
        [],
    )?;
    migrate(&mut db)?;
    let built = shoal_protocol::surfaces::Surface::choices(&[], Some("8080"), Some("Free here"));
    assert_eq!(interaction(&db, 5)?.0.as_ref(), Some(built.as_json()));
    Ok(())
}

#[test]
fn picks_become_answers_to_the_other_side_s_choices() -> Result<()> {
    let mut db = at_floor()?;
    thread_with_picks(&db)?;
    migrate(&mut db)?;
    assert_eq!(
        interaction(&db, 2)?.1,
        Some(serde_json::json!({"post": 1, "action": "2"}))
    );
    // Its own side's choices answer nothing.
    assert_eq!(interaction(&db, 3)?, (None, None));
    Ok(())
}

#[test]
fn a_holder_s_resolving_pick_becomes_the_holder_s_answer() -> Result<()> {
    let mut db = at_floor()?;
    // An agent asks the user, who holds the thread and resolves it.
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        INSERT INTO spaces(id,kind,repository_id,name) VALUES ('agent','chat','repo','asker');
        INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener,
                opener_id,outcome,resolved_by,resolved_at)
            VALUES (1,'agent',1,1,'question','Which database?',1,'space','agent',
                '{\"outcome\":\"answered\",\"choice\":2}','user',9);
        INSERT INTO thread_posts(id,thread_id,created_at,side,sender,sender_id,text,choices)
            VALUES (1,1,2,'opener','space','agent','Which?','[\"Postgres\",\"SQLite\"]');",
    )?;
    migrate(&mut db)?;
    let (id, side, sender, text): (i64, String, String, String) = db.query_row(
        "SELECT id,side,sender,text FROM thread_posts ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(
        (side.as_str(), sender.as_str(), text.as_str()),
        ("holder", "user", "SQLite")
    );
    assert_eq!(
        interaction(&db, id)?.1,
        Some(serde_json::json!({"post": 1, "action": "2"}))
    );
    Ok(())
}

#[test]
fn a_resolving_pick_answers_the_latest_choices_of_either_side() -> Result<()> {
    let mut db = at_floor()?;
    thread_with_picks(&db)?;
    // The opener offers choices after the holder's, and resolves with 2.
    db.execute_batch(
        "INSERT INTO thread_posts(id,thread_id,created_at,side,sender,text,choices)
            VALUES (5,1,6,'opener','user','Or the queue?','[\"Kafka\",\"NATS\"]');
        UPDATE threads SET outcome='{\"outcome\":\"answered\",\"choice\":2}' WHERE id=1;",
    )?;
    migrate(&mut db)?;
    let (id, text): (i64, String) = db.query_row(
        "SELECT id,text FROM thread_posts ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(text, "NATS");
    assert_eq!(
        interaction(&db, id)?.1,
        Some(serde_json::json!({"post": 5, "action": "2"}))
    );
    Ok(())
}

#[test]
fn the_hub_s_requests_and_summaries_take_the_new_shape_too() -> Result<()> {
    let mut db = at_floor()?;
    let summary = serde_json::json!({"id": "w1", "number": 1, "threads": [{
        "posts": [
            {"id": 1, "side": "holder", "text": "Which?", "choices": ["A", "B"]},
            {"id": 2, "side": "opener", "text": "B", "choice": 2},
        ],
        "resolution": {"outcome": {"outcome": "answered", "choice": 2}},
    }]});
    let queued = serde_json::json!({"post": {"text": "", "choice": 2}});
    db.execute_batch(&format!(
        "INSERT INTO hub_members(id,kind,name,public_key,joined_at)
            VALUES ('laptop','machine','laptop','key',1);
        INSERT INTO hub_spaces(space_id,member_id,summary,reported_at)
            VALUES ('w1','laptop','{summary}',1);
        INSERT INTO hub_queue(member_id,sender_id,call,summary,queued_at,expires_at)
            VALUES ('laptop','desk','{queued}','send',1,2);
        INSERT INTO hub_outbox(call,queued_at) VALUES ('{queued}',1);"
    ))?;
    migrate(&mut db)?;
    let read = |sql: &str| -> Result<serde_json::Value> {
        let text: String = db.query_row(sql, [], |row| row.get(0))?;
        Ok(serde_json::from_str(&text)?)
    };
    let summary = read("SELECT summary FROM hub_spaces")?;
    let thread = &summary["threads"][0];
    let built = shoal_protocol::surfaces::Surface::choices(&["A".into(), "B".into()], None, None);
    assert_eq!(&thread["posts"][0]["ui"], built.as_json());
    assert_eq!(thread["posts"][0].get("choices"), None);
    assert_eq!(
        thread["posts"][1]["answer"],
        serde_json::json!({"post": 1, "action": "2"})
    );
    assert_eq!(
        thread["resolution"]["outcome"],
        serde_json::json!({"outcome": "answered"})
    );
    let answer = serde_json::json!({"post": {"text": "", "answer": {"action": "2"}}});
    assert_eq!(read("SELECT call FROM hub_queue")?, answer);
    assert_eq!(read("SELECT call FROM hub_outbox")?, answer);
    Ok(())
}

#[test]
fn the_hub_s_kept_placement_requests_become_lasting_work() -> Result<()> {
    let mut db = at_floor()?;
    let start =
        serde_json::json!({"agent": "claude", "mode": "worker", "heavy": true, "caller": ""});
    db.execute_batch(&format!(
        "INSERT INTO hub_members(id,kind,name,public_key,joined_at)
            VALUES ('laptop','machine','laptop','key',1);
        INSERT INTO hub_packages(space_id,member_id,built_at,size,kept_at,start)
            VALUES ('w1','laptop',1,1,1,'{start}'), ('w2','laptop',1,1,1,NULL);"
    ))?;
    migrate(&mut db)?;
    let start_of = |id: &str| -> Result<Option<serde_json::Value>> {
        let sql = "SELECT start FROM hub_packages WHERE space_id=?1";
        let text: Option<String> = db.query_row(sql, [id], |row| row.get(0))?;
        Ok(text.map(|text| serde_json::from_str(&text)).transpose()?)
    };
    let mut lasting = start;
    lasting["lifetime"] = "lasting".into();
    assert_eq!(start_of("w1")?, Some(lasting));
    assert_eq!(start_of("w2")?, None);
    Ok(())
}

#[test]
fn a_resolution_s_pick_becomes_the_opener_s_answer_before_it() -> Result<()> {
    let mut db = at_floor()?;
    thread_with_picks(&db)?;
    let journaled = |db: &Connection| -> Result<i64> {
        Ok(db.query_row("SELECT COUNT(*) FROM space_events", [], |row| row.get(0))?)
    };
    let before = journaled(&db)?;
    migrate(&mut db)?;
    let (id, side, sender, text, created_at): (i64, String, String, String, i64) = db.query_row(
        "SELECT id,side,sender,text,created_at FROM thread_posts ORDER BY id DESC LIMIT 1",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    assert_eq!(
        (side.as_str(), sender.as_str(), text.as_str(), created_at),
        ("opener", "user", "Redis", 9)
    );
    assert_eq!(
        interaction(&db, id)?.1,
        Some(serde_json::json!({"post": 4, "action": "1"}))
    );
    let outcome: String = db.query_row("SELECT outcome FROM threads", [], |row| row.get(0))?;
    assert_eq!(outcome, "answered");
    assert_eq!(journaled(&db)?, before);
    // The journal records the posts that come after.
    db.execute(
        "INSERT INTO thread_posts(thread_id,created_at,side,sender,text) VALUES (1,10,'opener','user','Thanks')",
        [],
    )?;
    assert_eq!(journaled(&db)?, before + 1);
    Ok(())
}
