use super::*;
use crate::daemon::test_support::manager;

/// The spaces with events, in the order of their first one.
fn spaces(db: &Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT space_id FROM workspace_events GROUP BY space_id ORDER BY MIN(id)")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Journal `kind` for the workspace `id`, `age` seconds ago.
fn journal(db: &Connection, id: &str, kind: &str, age: i64) -> Result<()> {
    db.execute(
        "INSERT INTO workspace_events(created_at,record)
            VALUES (unixepoch()-?3,json_object('kind',?2,'workspace_id',?1))",
        params![id, kind, age],
    )?;
    Ok(())
}

#[tokio::test]
async fn gone_spaces_keep_their_events_for_the_retention_after_their_removal() {
    let (_temp, manager) = manager().await;
    let decade = 3651 * 24 * 3600;
    manager
        .store
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
                "INSERT INTO removed_workspaces(workspace_id,repository_id,name,expires_at,record)
                    VALUES ('restorable','repo','restorable',unixepoch()+60,'{}')",
                [],
            )?;
            db.execute(
                "INSERT INTO workspace_events(created_at,record) VALUES (unixepoch()-?1,
                    json_object('kind','chat_ended','chat',json_object('id','chat')))",
                [decade],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_events().await.unwrap();
    let left = manager.store.run(|db| spaces(db)).await.unwrap();
    assert_eq!(left, ["just-removed", "restorable"]);
}

#[tokio::test]
async fn a_restored_workspace_takes_the_removed_ones_events() {
    let (_temp, manager) = manager().await;
    let left = manager
        .store
        .run(|db| {
            journal(db, "removed", "removed", 0)?;
            journal(db, "other", "created", 0)?;
            journal(db, "restored", "created", 0)?;
            adopt(db, &"removed".into(), &"restored".into())?;
            spaces(db)
        })
        .await
        .unwrap();
    assert_eq!(left, ["restored", "other"]);
}

#[tokio::test]
async fn shoal_s_own_events_stay_for_the_global_retention() {
    let (_temp, manager) = manager().await;
    let decade = 3651 * 24 * 3600;
    let shoal = |db: &Connection, version: &str, age: i64| {
        db.execute(
            "INSERT INTO workspace_events(created_at,record) VALUES (unixepoch()-?2,
                json_object('kind','daemon_started','version',?1))",
            params![version, age],
        )
    };
    manager
        .store
        .run(move |db| {
            shoal(db, "old", decade)?;
            shoal(db, "new", 0)?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_events().await.unwrap();
    let left: Vec<String> = (manager.store)
        .run(|db| {
            Ok(db
                .prepare("SELECT json_extract(record,'$.version') FROM workspace_events")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    assert_eq!(left, ["new"]);
}
