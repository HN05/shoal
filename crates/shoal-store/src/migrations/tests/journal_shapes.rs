//! Migration 303: each journal record takes its kind's one shape.
use super::*;
use serde_json::{Value, json};
use shoal_protocol::events::{EventDetails, SpaceEvent};

/// A chat's creation as an earlier Shoal journaled it: with its agent and
/// without a parent.
fn chat_created_with_agent() -> Value {
    json!({
        "kind": "created", "space_id": "chat", "space_kind": "chat",
        "repository_id": null, "name": "planner", "cause": null, "error": null,
        "base_ref": null, "base_workspace": null,
        "agent": {"name": "claude", "execution_id": null, "exit_code": null},
    })
}

/// A workspace's creation from before creations recorded their base ref.
fn workspace_created_without_base_ref() -> Value {
    json!({
        "kind": "created", "space_id": "work", "space_kind": "workspace",
        "repository_id": "repo", "name": "fix", "path": "/fix", "branch": "fix",
        "cause": null, "error": null, "base_workspace": null,
        "parent": {"id": "chat", "name": "planner", "kind": "chat"},
    })
}

/// A conversion without a handoff, as its trigger journaled it.
fn converted_without_handoff() -> Value {
    json!({
        "kind": "converted", "space_id": "work", "space_kind": "workspace",
        "repository_id": "repo", "name": "fix", "path": "/fix", "branch": "fix",
        "cause": null, "error": null,
    })
}

/// The records the journal holds after migrating `records` from the floor.
fn migrated_records(records: &[Value]) -> Result<Vec<Value>> {
    let mut db = at_floor()?;
    for record in records {
        db.execute(
            "INSERT INTO space_events(space_id,record) VALUES (?1,?2)",
            rusqlite::params![record["space_id"].as_str(), record.to_string()],
        )?;
    }
    migrate(&mut db)?;
    let mut statement = db.prepare("SELECT record FROM space_events ORDER BY id")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|text| Ok(serde_json::from_str(&text?)?)).collect()
}

/// `record` decoded, after checking that encoding it again gives it back.
fn decoded(record: &Value) -> Result<EventDetails> {
    let details: EventDetails = serde_json::from_value(record.clone())?;
    assert_eq!(&serde_json::to_value(&details)?, record);
    Ok(details)
}

#[test]
fn legacy_creations_and_conversions_take_their_kind_s_shape() -> Result<()> {
    let records = migrated_records(&[
        chat_created_with_agent(),
        workspace_created_without_base_ref(),
        converted_without_handoff(),
    ])?;
    let [chat, workspace, converted] = &records[..] else {
        anyhow::bail!("expected three records, found {records:?}");
    };

    assert_eq!(chat.get("agent"), None);
    let SpaceEvent::Created { parent, .. } = decoded(chat)?.event else {
        anyhow::bail!("not a creation: {chat}");
    };
    assert_eq!(parent, None);

    let SpaceEvent::Created {
        base_ref, parent, ..
    } = decoded(workspace)?.event
    else {
        anyhow::bail!("not a creation: {workspace}");
    };
    assert_eq!(base_ref, None);
    assert_eq!(
        parent.map(|parent| parent.id.to_string()),
        Some("chat".into())
    );

    let SpaceEvent::Converted { handoff } = decoded(converted)?.event else {
        anyhow::bail!("not a conversion: {converted}");
    };
    assert!(handoff.is_none());
    Ok(())
}

#[test]
fn records_of_other_kinds_keep_their_shape() -> Result<()> {
    let renamed = json!({
        "kind": "renamed", "space_id": "work", "space_kind": "workspace",
        "repository_id": "repo", "name": "fix", "path": "/fix", "branch": "fix",
        "cause": null, "error": null,
    });
    assert_eq!(migrated_records(std::slice::from_ref(&renamed))?, [renamed]);
    Ok(())
}

#[test]
fn payload_fields_the_types_leave_out_when_empty_go_and_thread_files_take_their_source()
-> Result<()> {
    let file = json!({
        "kind": "file_added", "space_id": "work", "repository_id": "repo",
        "file": {"id": 1, "space": {"id": "work", "name": "fix", "kind": "workspace"},
            "source": "question", "name": "notes.md", "title": null, "session": null,
            "path": "/notes.md", "bytes": 3, "created_at": 1, "attached_by": {"kind": "user"}},
    });
    let post = json!({
        "kind": "thread_posted", "space_id": "work", "space_kind": "workspace",
        "repository_id": "repo", "name": "fix", "path": "/fix", "branch": "fix",
        "cause": null, "error": null,
        "thread": {"space_number": 2, "item": 1, "number": "2.1", "name": "Which?",
            "text": "Postgres", "sender": "the user", "to": null},
    });
    let records = migrated_records(&[file, post])?;
    let [file, post] = &records[..] else {
        anyhow::bail!("expected two records, found {records:?}");
    };
    assert_eq!(
        (&file["file"]["source"], file["file"].get("title")),
        (&json!("thread"), None)
    );
    let file: shoal_protocol::events::FileEventDetails = serde_json::from_value(file.clone())?;
    assert_eq!(serde_json::to_value(&file)?, records[0]);
    assert_eq!(post["thread"].get("to"), None);
    assert_eq!(post["thread"]["text"], "Postgres");
    decoded(post)?;
    Ok(())
}
