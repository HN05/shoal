//! Migration 325: each agent event names the mode its agent ran in.
use super::*;
use serde_json::{Value, json};

#[test]
fn an_agent_event_takes_its_execution_s_mode_or_null() -> Result<()> {
    let mut db = at_floor()?;
    db.execute(
        "INSERT INTO executions(id,space_id,state,agent,mode)
            VALUES ('kept','work','running','claude','plan')",
        [],
    )?;
    let agent = |kind: &str, execution: &str| {
        json!({"kind": kind, "space_id": "work", "repository_id": "repo",
            "agent": {"name": "claude", "execution_id": execution, "exit_code": null}})
    };
    for record in [
        agent("agent_started", "kept"),
        agent("agent_exited", "gone"),
        json!({"kind": "renamed", "space_id": "work", "repository_id": "repo"}),
    ] {
        db.execute(
            "INSERT INTO space_events(space_id,record) VALUES ('work',?1)",
            [record.to_string()],
        )?;
    }
    migrate(&mut db)?;
    let mut statement = db.prepare("SELECT record FROM space_events ORDER BY id")?;
    let records = statement.query_map([], |row| row.get::<_, String>(0))?;
    let modes = (records.collect::<rusqlite::Result<Vec<_>>>()?.into_iter())
        .map(|record| Ok(serde_json::from_str::<Value>(&record)?["agent"]["mode"].clone()))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(modes, [json!("plan"), Value::Null, Value::Null]);
    Ok(())
}
