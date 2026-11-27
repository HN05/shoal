//! Migration 337: a launch's plan leaves out what its row's columns hold.
use super::*;
use serde_json::{Value, json};

/// A database at the floor with the chat `chat`, whose failed launch the
/// tests record.
fn with_chat() -> Result<Connection> {
    let db = at_floor()?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO spaces(id,kind,name,mode) VALUES ('chat','chat','planner','chat');
         INSERT INTO chats(id) VALUES ('chat');
         COMMIT;",
    )?;
    Ok(db)
}

/// Record the queued launch `id` with `plan` and the columns an earlier
/// Shoal filled from the same launch.
fn queue_launch(db: &Connection, id: &str, plan: &Value) -> Result<()> {
    db.execute(
        "INSERT INTO agent_queue(id,kind,name,agent,issue_title,issue_labels,plan,env,state,
            created_at,mode,narrowing)
         VALUES (?1,'launch','fix','claude','Fix it','[\"bug\"]',?2,'{}','queued',1,'plan',
            '{\"merge\":\"direct\"}')",
        rusqlite::params![id, plan.to_string()],
    )?;
    Ok(())
}

/// The plan of `table`'s row `id`, and its issue's title and labels.
fn stored(db: &Connection, table: &str, id: &str) -> Result<(Value, Option<String>, String)> {
    let (plan, title, labels): (String, Option<String>, String) = db.query_row(
        &format!("SELECT plan,issue_title,issue_labels FROM {table} WHERE id=?1"),
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok((serde_json::from_str(&plan)?, title, labels))
}

/// An earlier Shoal's plan of a launch on issue 7, with every copy its
/// columns hold.
fn plan_on_issue() -> Value {
    json!({
        "launch": {
            "agent": "claude", "prompt": "Fix it", "mode": "plan",
            "narrowing": {"merge": "direct"}, "source": {"kind": "manual"},
            "issue": {"url": "https://forge.example/team/repo/issues/7",
                "title": "Fix it", "labels": ["bug"]},
            "args": ["--fast"],
        },
        "pr": "8",
    })
}

#[test]
fn a_plan_keeps_only_what_its_columns_do_not() -> Result<()> {
    let mut db = with_chat()?;
    queue_launch(&db, "issue", &plan_on_issue())?;
    queue_launch(
        &db,
        "bare",
        &json!({"launch": {"agent": "claude", "prompt": null}}),
    )?;
    db.execute(
        "INSERT INTO failed_launches(id,space_id,name,agent,plan,env,created_at,error,failed_at)
         VALUES ('failed','chat','planner','claude',?1,'{}',1,'gone',2)",
        [plan_on_issue().to_string()],
    )?;

    migrate(&mut db)?;

    let rewritten = json!({
        "launch": {
            "prompt": "Fix it", "args": ["--fast"],
            "issue": "https://forge.example/team/repo/issues/7",
        },
        "pr": "8",
    });
    let issue_columns = (Some("Fix it".to_owned()), r#"["bug"]"#.to_owned());
    let (plan, title, labels) = stored(&db, "agent_queue", "issue")?;
    assert_eq!(plan, rewritten);
    assert_eq!((title, labels), issue_columns);
    let (plan, ..) = stored(&db, "agent_queue", "bare")?;
    assert_eq!(plan, json!({"launch": {"prompt": null}}));
    // A failed launch's columns take its issue's title and labels.
    let (plan, title, labels) = stored(&db, "failed_launches", "failed")?;
    assert_eq!(plan, rewritten);
    assert_eq!((title, labels), issue_columns);
    Ok(())
}

#[test]
fn a_plan_without_its_agent_stops_the_migration_naming_its_row() -> Result<()> {
    let mut db = with_chat()?;
    queue_launch(&db, "agentless", &json!({"launch": {"prompt": "Fix it"}}))?;

    let error = format!("{:#}", migrate(&mut db).unwrap_err());

    assert!(error.contains("agent_queue row agentless"), "{error}");
    assert!(error.contains("names no agent"), "{error}");
    Ok(())
}
