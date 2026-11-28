use super::*;
use futures_util::future::BoxFuture;
use rusqlite::params;
use shoal_protocol::events::{
    EventDetails, EventSpace, ShoalEventDetails, ShoalEventKind, SpaceEvent,
};
use shoal_protocol::test_support;
use shoal_store::{journal, test_support::seeds};
use std::path::Path;
use std::time::Duration;

/// A removed space's events stay a month, and Shoal's own a year.
struct Fixed;

impl Retention for Fixed {
    fn of<'a>(&'a self, repository: Option<&'a RepositoryId>) -> BoxFuture<'a, Duration> {
        let days = if repository.is_some() { 30 } else { 365 };
        Box::pin(async move { Duration::from_secs(days * 24 * 3600) })
    }
}

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

/// The spaces with events, in the order of their first one.
fn spaces(db: &Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT space_id FROM space_events GROUP BY space_id ORDER BY MIN(id)")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Journal `details`, `age` seconds ago.
fn journal_at(db: &Connection, details: &EventDetails, age: i64) -> Result<()> {
    let row = seeds::space_event(db, details)?;
    db.execute(
        "UPDATE space_events SET created_at=unixepoch()-?2 WHERE id=?1",
        params![row, age],
    )?;
    Ok(())
}

/// Journal `event` for the workspace `id`, `age` seconds ago.
fn journal(db: &Connection, id: &str, event: SpaceEvent, age: i64) -> Result<()> {
    let details = test_support::workspace_event(id, id, Path::new("/work"), event);
    journal_at(db, &details, age)
}

fn created() -> SpaceEvent {
    SpaceEvent::Created {
        base_ref: None,
        base_workspace: None,
        parent: None,
    }
}

#[tokio::test]
async fn gone_spaces_keep_their_events_for_the_retention_after_their_removal() {
    let (_temp, store) = store().await;
    let decade = 3651 * 24 * 3600;
    store
        .run(move |db| {
            journal(db, "long-gone", created(), decade + 60)?;
            journal(db, "long-gone", SpaceEvent::Removed, decade)?;
            // A later event of a gone space, such as one its kept files
            // record as they expire, does not extend its retention.
            journal(db, "long-gone", SpaceEvent::Renamed, 0)?;
            journal(db, "just-removed", SpaceEvent::Removed, 0)?;
            journal(db, "restorable", SpaceEvent::Removed, decade)?;
            let repository = test_support::repository("repo", Path::new("/repo"));
            seeds::repository(db, &repository)?;
            db.execute(
                "INSERT INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
                    VALUES ('restorable','repo',0,'restorable',unixepoch()+60,'{}')",
                [],
            )?;
            let chat = EventDetails {
                space: EventSpace::Chat,
                ..test_support::workspace_event("chat", "chat", Path::new("/"), SpaceEvent::Removed)
            };
            journal_at(db, &chat, decade)
        })
        .await
        .unwrap();
    expire(&store, &Fixed).await.unwrap();
    let left = store.run(|db| spaces(db)).await.unwrap();
    assert_eq!(left, ["just-removed", "restorable"]);
}

#[tokio::test]
async fn shoal_s_own_events_stay_for_the_global_retention() {
    let (_temp, store) = store().await;
    let decade = 3651 * 24 * 3600;
    let shoal = |db: &Connection, version: &str, age: i64| {
        let mut started = ShoalEventDetails::new(ShoalEventKind::DaemonStarted);
        started.version = Some(version.into());
        journal::append_shoal(db, &started)?;
        db.execute(
            "UPDATE space_events SET created_at=unixepoch()-?2 WHERE id=?1",
            params![db.last_insert_rowid(), age],
        )?;
        anyhow::Ok(())
    };
    store
        .run(move |db| {
            shoal(db, "old", decade)?;
            shoal(db, "new", 0)?;
            Ok(())
        })
        .await
        .unwrap();
    expire(&store, &Fixed).await.unwrap();
    let left: Vec<String> = store
        .run(|db| {
            Ok(db
                .prepare("SELECT record FROM space_events")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    let left: Vec<_> = (left.iter())
        .map(|record| serde_json::from_str::<ShoalEventDetails>(record).unwrap())
        .map(|details| details.version)
        .collect();
    assert_eq!(left, [Some("new".to_owned())]);
}
