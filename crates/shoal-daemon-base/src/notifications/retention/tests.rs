use super::*;
use futures_util::future::BoxFuture;
use shoal_store::test_support::seeds;
use std::time::Duration;

const DAY: i64 = 24 * 3600;

/// The repository `short` keeps notifications two days, every other one
/// a hundred, and the global setting ten.
struct Fixed;

impl Retention for Fixed {
    fn of<'a>(&'a self, repository: Option<&'a RepositoryId>) -> BoxFuture<'a, Duration> {
        let days = match repository.map(|id| id.as_str()) {
            Some("short") => 2,
            Some(_) => 100,
            None => 10,
        };
        Box::pin(async move { Duration::from_secs(days * 24 * 3600) })
    }
}

/// The notifications of a store in a temporary state directory, with the
/// repositories `short` and `long`, the workspace `in-short` of the first,
/// `in-long` of the second and the removed workspace `removed-short` of
/// the first.
async fn notifications() -> (tempfile::TempDir, Notifications) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    store
        .run(|db| {
            seeds::ready_workspace(db, "short", "in-short", "a")?;
            seeds::ready_workspace(db, "long", "in-long", "b")?;
            db.execute_batch(
                "INSERT INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
                    VALUES ('removed-short','short',0,'c',unixepoch()+60,'{}');",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (temp, Notifications::new(store))
}

/// Record the notification `message` about the space `space`, `days` ago.
async fn record(notifications: &Notifications, space: Option<&str>, message: &str, days: i64) {
    let (space, message) = (space.map(str::to_owned), message.to_owned());
    notifications
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO alerts(created_at,kind,sender,space_id,message)
                    VALUES (unixepoch()-?1,'workspace_removed','shoal',?2,?3)",
                params![days * DAY, space, message],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The messages of the notifications left, oldest first.
async fn left(notifications: &Notifications) -> Vec<String> {
    let all = notifications.list(false, u32::MAX).await.unwrap();
    all.into_iter().map(|alert| alert.message).collect()
}

#[tokio::test]
async fn notifications_about_a_space_stay_for_its_repository_s_retention() {
    let (_temp, notifications) = notifications().await;
    record(&notifications, Some("in-short"), "expired unread", 3).await;
    record(&notifications, Some("removed-short"), "expired removed", 3).await;
    record(&notifications, Some("in-long"), "kept by its repository", 3).await;
    record(&notifications, Some("in-short"), "recent", 1).await;
    notifications.mark_read(vec![2]).await.unwrap();
    notifications.expire(&Fixed).await.unwrap();
    assert_eq!(
        left(&notifications).await,
        ["kept by its repository", "recent"]
    );
}

#[tokio::test]
async fn notifications_without_a_repository_stay_for_the_global_retention() {
    let (_temp, notifications) = notifications().await;
    record(&notifications, None, "expired", 11).await;
    record(&notifications, Some("gone"), "expired gone", 11).await;
    record(&notifications, None, "recent", 9).await;
    record(&notifications, Some("gone"), "recent gone", 3).await;
    notifications.expire(&Fixed).await.unwrap();
    assert_eq!(left(&notifications).await, ["recent", "recent gone"]);
}
