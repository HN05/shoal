use super::*;
use crate::store::Store;
use anyhow::Result;

/// A store in a temporary state directory.
async fn store() -> (tempfile::TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    (
        temp,
        shoal_store::test_support::unsynced_store(path)
            .await
            .unwrap(),
    )
}

#[tokio::test]
async fn an_agent_ends_once_whether_a_stop_or_its_exit_reports_it() {
    let (_temp, store) = store().await;
    let agent = |execution: &str, exit_code| AgentEvent {
        source: None,
        name: "claude".into(),
        execution_id: Some(execution.into()),
        exit_code,
        oom_kills: 0,
    };
    let recorded = [
        (EventKind::AgentStarted, agent("e1", None)),
        (EventKind::AgentStopped, agent("e1", None)),
        // The wrapper of the stopped agent then ends.
        (EventKind::AgentStopped, agent("e1", None)),
        (EventKind::AgentExited, agent("e1", Some(0))),
        (EventKind::AgentStarted, agent("e1", None)),
        (EventKind::AgentExited, agent("e1", Some(3))),
        (EventKind::AgentStarted, agent("e2", None)),
    ];
    let events = store
        .run(move |db| {
            db.execute_batch(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('r','/r','/r',1);
                 BEGIN;
                 INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
                     VALUES ('w','/work','worker','ready','abc','/admin');
                 INSERT INTO spaces(id,kind,repository_id,name)
                     VALUES ('w','workspace','r','worker');
                 COMMIT;",
            )?;
            for (kind, agent) in &recorded {
                insert_agent(db, &SpaceId::from("w"), *kind, agent)?;
            }
            Ok(())
        })
        .await;
    events.unwrap();
    let (items, _) = page(&store, None, None, 100).await.unwrap();
    let agents: Vec<_> = items
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) => event.details.agent.map(|agent| {
                (
                    event.details.kind,
                    agent.execution_id.unwrap().to_string(),
                    agent.exit_code,
                )
            }),
            _ => None,
        })
        .collect();
    assert_eq!(
        agents,
        [
            (EventKind::AgentStarted, "e1".to_owned(), None),
            (EventKind::AgentStopped, "e1".to_owned(), None),
            (EventKind::AgentStarted, "e1".to_owned(), None),
            (EventKind::AgentExited, "e1".to_owned(), Some(3)),
            (EventKind::AgentStarted, "e2".to_owned(), None),
        ]
    );
}

#[test]
fn link_events_include_item_kind_and_url() -> Result<()> {
    let db = shoal_store::test_support::migrated()?;
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO workspaces(id,path,branch,state,observed_branch,base_commit,git_dir)
            VALUES ('w','/work','worker','ready','worker','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','worker');
         COMMIT;",
    )?;
    super::record_link(
        &db,
        &SpaceId::from("w"),
        super::EventKind::Linked,
        crate::model::Relation::Assigned,
        crate::forge::items::link::ItemKind::Pr,
        "https://forge.example/team/repo/pulls/7",
    )?;
    let record: String = db.query_row(
        "SELECT record FROM space_events WHERE json_extract(record,'$.kind')='linked'",
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
fn item_activity_events_include_item_and_update_details() -> Result<()> {
    let db = shoal_store::test_support::migrated()?;
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO workspaces(id,path,branch,state,observed_branch,base_commit,git_dir)
            VALUES ('w','/work','worker','ready','worker','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','worker');
         COMMIT;",
    )?;
    let update = crate::forge::updates::Update {
        url: "https://forge.example/team/repo/pulls/7".into(),
        kind: crate::forge::updates::UpdateKind::Comment,
        message: "read the new review".into(),
        delivery: String::new(),
        check: None,
        workspace: None,
    };
    super::record_item_activity(
        &db,
        &SpaceId::from("w"),
        crate::forge::items::link::LinkKind::Pr,
        &update.url,
        &update,
    )?;
    let record: String = db.query_row(
        "SELECT record FROM space_events WHERE json_extract(record,'$.kind')='item_activity'",
        [],
        |row| row.get(0),
    )?;
    let record: serde_json::Value = serde_json::from_str(&record)?;
    assert_eq!(record["item_activity"]["kind"], "pr");
    assert_eq!(record["item_activity"]["url"], update.url);
    assert_eq!(record["item_activity"]["update"]["kind"], "comment");
    assert_eq!(record["item_activity"]["update"]["message"], update.message);
    Ok(())
}

#[test]
fn run_link_activity_and_approval_events_read_back() -> Result<()> {
    use super::EventDetails;
    use crate::forge::items::link::LinkKind;
    let db = shoal_store::test_support::migrated()?;
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO workspaces(id,path,branch,state,observed_branch,base_commit,git_dir)
            VALUES ('w','/work','worker','ready','worker','abc','/admin');
         INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','worker');
         COMMIT;",
    )?;
    let url = "https://forge.example/team/repo/actions/runs/2765";
    super::record_link(
        &db,
        &SpaceId::from("w"),
        super::EventKind::Linked,
        crate::model::Relation::Related,
        LinkKind::Ci,
        url,
    )?;
    let update = crate::forge::updates::Update {
        url: url.into(),
        kind: crate::forge::updates::UpdateKind::CiCompleted,
        message: "Run finished".into(),
        delivery: String::new(),
        check: None,
        workspace: None,
    };
    super::record_item_activity(&db, &SpaceId::from("w"), LinkKind::Ci, url, &update)?;
    super::record_item(&db, &SpaceId::from("w"), LinkKind::Ci, url, "approve")?;
    let records = db
        .prepare("SELECT record FROM space_events ORDER BY id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let events = records
        .iter()
        .map(|record| serde_json::from_str::<EventDetails>(record))
        .collect::<serde_json::Result<Vec<_>>>()?;
    let kinds = events
        .iter()
        .filter_map(|event| {
            let link = event.link.as_ref().map(|link| link.kind);
            let item = event.item.as_ref().map(|item| item.kind);
            let activity = event.item_activity.as_ref().map(|activity| activity.kind);
            link.or(item).or(activity)
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, [LinkKind::Ci, LinkKind::Ci, LinkKind::Ci]);
    Ok(())
}

#[test]
fn chat_events_reach_the_journal_under_the_chat() -> Result<()> {
    use super::{EventDetails, EventKind};
    let db = shoal_store::test_support::migrated()?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
         INSERT INTO chats(id)
             VALUES ('c');
         INSERT INTO spaces(id,kind,repository_id,name,mode)
             VALUES ('c','chat','repo','planner','chat');
         INSERT INTO space_review(space_id,url,kind) VALUES ('c','',NULL);
         COMMIT;",
    )?;
    super::record_link(
        &db,
        &SpaceId::from("c"),
        EventKind::Linked,
        crate::model::Relation::Assigned,
        crate::forge::items::link::LinkKind::Issue,
        "https://forge.example/team/repo/issues/1",
    )?;
    db.execute_batch(
        "INSERT INTO space_completion(space_id,record)
             VALUES ('c','{\"head\":\"abc\",\"cleanup\":true,\"error\":null}');
         DELETE FROM space_completion WHERE space_id='c';",
    )?;
    let rows = db
        .prepare("SELECT space_id,record FROM space_events ORDER BY id")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let events = rows
        .iter()
        .map(|(space, record)| {
            Ok((
                space.as_str(),
                serde_json::from_str::<EventDetails>(record)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let kinds: Vec<_> = events.iter().map(|(_, event)| event.kind).collect();
    assert_eq!(
        kinds,
        [
            EventKind::Created,
            EventKind::ReviewReady,
            EventKind::Linked,
            EventKind::Completed,
            EventKind::Undone
        ]
    );
    for (space, event) in &events {
        assert_eq!(*space, "c");
        assert_eq!(event.space_id.as_str(), "c");
        assert_eq!(event.space, shoal_protocol::events::EventSpace::Chat);
        assert_eq!(event.name, "planner");
    }
    for (_, record) in &rows {
        let record: serde_json::Value = serde_json::from_str(record)?;
        assert_eq!(
            (record.get("path"), record.get("branch")),
            (None, None),
            "{record}"
        );
    }
    Ok(())
}
