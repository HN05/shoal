use anyhow::Result;
use rusqlite::Connection;
use shoal_ids::{
    agent_session_ids::AgentSessionId, execution_ids::ExecutionId, space_ids::SpaceId,
};
use shoal_protocol::{integrations::IntegrationKind, transcripts::HandedOverSession};

use super::{continued_from, record, started};

/// A chat without a repository, whose agent `codex` runs as `codex-run`.
fn chat() -> Result<Connection> {
    let db = crate::test_support::migrated()?;
    db.execute_batch(
        "INSERT INTO spaces(id,kind,name,mode) VALUES ('chat','chat','chat','chat');
         INSERT INTO chats(id) VALUES ('chat');
         INSERT INTO executions(id,space_id,state,agent) VALUES ('codex-run','chat','running','codex');",
    )?;
    Ok(db)
}

fn side(agent: &str, session: Option<&str>, integration: IntegrationKind) -> HandedOverSession {
    HandedOverSession {
        agent: agent.parse().unwrap(),
        session: session.map(AgentSessionId::from),
        integration,
    }
}

fn space() -> SpaceId {
    SpaceId::from("chat".to_owned())
}

#[test]
fn the_new_tools_first_session_completes_the_handoff_and_journals_it() -> Result<()> {
    let db = chat()?;
    let from = side("claude", Some("before"), IntegrationKind::Claude);
    let to = side("codex", None, IntegrationKind::Codex);
    record(&db, &space(), &from, &to)?;
    let execution = ExecutionId::from("codex-run".to_owned());
    started(&db, &space(), &execution, &AgentSessionId::from("after"))?;
    // A later session of the same agent starts nothing.
    started(&db, &space(), &execution, &AgentSessionId::from("later"))?;

    let continued = continued_from(&db, &space())?;
    assert_eq!(continued.len(), 1);
    assert_eq!(continued[&AgentSessionId::from("after")], from);
    let events: Vec<String> = db
        .prepare("SELECT record FROM space_events WHERE kind='converted'")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let [event] = events.as_slice() else {
        panic!("one converted event: {events:?}");
    };
    let event: serde_json::Value = serde_json::from_str(event)?;
    assert_eq!(
        event["handoff"],
        serde_json::json!({
            "from": {"agent": "claude", "session": "before", "integration": "claude"},
            "to": {"agent": "codex", "session": "after", "integration": "codex"},
        })
    );
    assert_eq!(event["space_kind"], "chat");
    let details: shoal_protocol::events::EventDetails = serde_json::from_value(event)?;
    let shoal_protocol::events::SpaceEvent::Converted { handoff } = details.event else {
        anyhow::bail!("not a conversion: {:?}", details.event);
    };
    assert_eq!(
        handoff.map(|handoff| handoff.to.session),
        Some(Some("after".into()))
    );
    Ok(())
}

#[test]
fn a_session_of_another_agent_leaves_the_handoff_waiting() -> Result<()> {
    let db = chat()?;
    db.execute(
        "INSERT INTO executions(id,space_id,state,agent) VALUES ('claude-run','chat','running','claude')",
        [],
    )?;
    let from = side("claude", Some("before"), IntegrationKind::Claude);
    record(
        &db,
        &space(),
        &from,
        &side("codex", None, IntegrationKind::Codex),
    )?;
    let claude = ExecutionId::from("claude-run".to_owned());
    started(&db, &space(), &claude, &AgentSessionId::from("other"))?;
    assert!(continued_from(&db, &space())?.is_empty());
    Ok(())
}

#[test]
fn a_new_handoff_drops_one_that_never_started_a_session() -> Result<()> {
    let db = chat()?;
    let claude = side("claude", Some("before"), IntegrationKind::Claude);
    let gemini = side("gemini", None, IntegrationKind::None);
    record(&db, &space(), &claude, &gemini)?;
    record(
        &db,
        &space(),
        &claude,
        &side("codex", None, IntegrationKind::Codex),
    )?;
    let count: i64 = db.query_row("SELECT COUNT(*) FROM agent_handoffs", [], |row| row.get(0))?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn handoffs_go_with_their_space() -> Result<()> {
    let db = chat()?;
    let from = side("claude", Some("before"), IntegrationKind::Claude);
    record(
        &db,
        &space(),
        &from,
        &side("codex", None, IntegrationKind::Codex),
    )?;
    db.execute("DELETE FROM spaces WHERE id='chat'", [])?;
    let count: i64 = db.query_row("SELECT COUNT(*) FROM agent_handoffs", [], |row| row.get(0))?;
    assert_eq!(count, 0);
    Ok(())
}
