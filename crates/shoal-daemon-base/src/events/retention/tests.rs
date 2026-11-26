use super::*;
use futures_util::future::BoxFuture;
use rusqlite::params;
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

/// Journal `kind` for the workspace `id`, `age` seconds ago.
fn journal(db: &Connection, id: &str, kind: &str, age: i64) -> Result<()> {
    db.execute(
        "INSERT INTO space_events(created_at,space_id,kind,bulk,record)
            VALUES (unixepoch()-?3,?1,?2,0,
                json_object('kind',?2,'space_id',?1,'space_kind','workspace'))",
        params![id, kind, age],
    )?;
    Ok(())
}

#[tokio::test]
async fn gone_spaces_keep_their_events_for_the_retention_after_their_removal() {
    let (_temp, store) = store().await;
    let decade = 3651 * 24 * 3600;
    store
        .run(move |db| {
            journal(db, "long-gone", "created", decade + 60)?;
            journal(db, "long-gone", "removed", decade)?;
            // A later event of a gone space, such as its kept files
            // expiring, does not extend its retention.
            journal(db, "long-gone", "file_removed", 0)?;
            journal(db, "just-removed", "removed", 0)?;
            journal(db, "restorable", "removed", decade)?;
            db.execute(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
                [],
            )?;
            db.execute(
                "INSERT INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
                    VALUES ('restorable','repo',0,'restorable',unixepoch()+60,'{}')",
                [],
            )?;
            db.execute(
                "INSERT INTO space_events(created_at,space_id,kind,bulk,record)
                    VALUES (unixepoch()-?1,'chat','removed',0,
                        json_object('kind','removed','space_id','chat','space_kind','chat'))",
                [decade],
            )?;
            Ok(())
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
        db.execute(
            "INSERT INTO space_events(created_at,kind,bulk,record) VALUES (unixepoch()-?2,
                'daemon_started',0,json_object('kind','daemon_started','version',?1))",
            params![version, age],
        )
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
                .prepare("SELECT json_extract(record,'$.version') FROM space_events")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    assert_eq!(left, ["new"]);
}
