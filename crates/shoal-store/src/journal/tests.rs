use super::*;
use crate::test_support::migrated;
use serde_json::Value;
use shoal_protocol::model::{RefPart, SpaceRef};

mod triggers;

/// A repository with the workspaces `base` and `work`, which builds on
/// `base`, and the chat `chat`.
fn spaces() -> Result<Connection> {
    let db = migrated()?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
             VALUES ('base','/base','api','ready','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('base','workspace','repo','api');
         INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir,base_workspace_id)
             VALUES ('work','/work','fix','ready','abc','/admin','base');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('work','workspace','repo','fix');
         INSERT INTO chats(id) VALUES ('chat');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('chat','chat',NULL,'planner');
         DELETE FROM space_events;
         COMMIT;",
    )?;
    Ok(db)
}

/// The journal's records, each decoded after checking that encoding it
/// again gives it back.
fn journaled(db: &Connection) -> Result<Vec<EventDetails>> {
    let mut statement = db.prepare("SELECT space_id,record FROM space_events ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, SpaceId>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (space, text) = row?;
        let stored: Value = serde_json::from_str(&text)?;
        let details: EventDetails = serde_json::from_value(stored.clone())?;
        assert_eq!(serde_json::to_value(&details)?, stored);
        assert_eq!(details.space_id, space);
        Ok(details)
    })
    .collect()
}

#[test]
fn an_event_records_its_space_as_it_is() -> Result<()> {
    let db = spaces()?;
    append(&db, &"work".into(), SpaceEvent::Renamed)?;
    append_caused(
        &db,
        &"chat".into(),
        SpaceEvent::Removed,
        Some(EventCause::Idle),
        Some("busy"),
    )?;
    let [work, chat] = &journaled(&db)?[..] else {
        anyhow::bail!("expected two events");
    };
    assert_eq!(
        (
            work.space.clone(),
            work.repository_id.clone(),
            work.name.to_string()
        ),
        (
            EventSpace::Workspace {
                path: "/work".into(),
                branch: Some("fix".parse()?),
            },
            Some("repo".into()),
            "fix".to_owned()
        )
    );
    assert_eq!((work.cause, work.error.as_deref()), (None, None));
    assert_eq!(
        (chat.space.clone(), chat.repository_id.clone(), chat.kind()),
        (
            EventSpace::Chat,
            None,
            shoal_protocol::events::EventKind::Removed
        )
    );
    assert_eq!(
        (chat.cause, chat.error.as_deref()),
        (Some(EventCause::Idle), Some("busy"))
    );
    Ok(())
}

#[test]
fn a_space_that_is_gone_records_nothing() -> Result<()> {
    let db = spaces()?;
    append(&db, &"gone".into(), SpaceEvent::Renamed)?;
    assert!(journaled(&db)?.is_empty());
    Ok(())
}

#[test]
fn a_chat_leaves_out_what_only_a_workspace_records() -> Result<()> {
    let db = spaces()?;
    let parent = SpaceRef {
        id: "base".into(),
        name: "api".parse()?,
        part: RefPart::Workspace { branch: None },
    };
    for id in ["chat", "work"] {
        append(&db, &id.into(), SpaceEvent::BranchChanged)?;
        let parent = Some(parent.clone());
        append(&db, &id.into(), SpaceEvent::ParentChanged { parent })?;
        append(&db, &id.into(), SpaceEvent::Undone)?;
    }
    let recorded: Vec<_> = (journaled(&db)?.into_iter())
        .map(|event| format!("{} {}", event.space_id, event.kind()))
        .collect();
    assert_eq!(
        recorded,
        [
            "chat undone",
            "work branch_changed",
            "work parent_changed",
            "work undone"
        ]
    );
    Ok(())
}

#[test]
fn a_workspace_s_base_is_named_while_it_builds_on_one() -> Result<()> {
    let db = spaces()?;
    let base = base_of(&db, &"work".into())?;
    assert_eq!(
        base,
        Some(WorkspaceRef {
            id: "base".into(),
            name: "api".parse()?,
            branch: Some("api".parse()?),
        })
    );
    assert_eq!(base_of(&db, &"base".into())?, None);
    Ok(())
}
