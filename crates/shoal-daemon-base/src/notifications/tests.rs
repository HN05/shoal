use super::*;

/// No space has a coordinator.
struct Nobody;

impl Coordinators for Nobody {
    fn tell<'a>(
        &'a self,
        _name: &'a SpaceName,
        _id: Option<&'a SpaceId>,
        _kind: AlertKind,
        _message: String,
        _files: Vec<PathBuf>,
        _source: Source,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
}

/// The notifications of a store in a temporary state directory.
async fn notifications() -> (tempfile::TempDir, Notifications) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    (temp, Notifications::new(store))
}

#[tokio::test]
async fn pruning_keeps_unread_and_the_newest_read_notifications() {
    let (_temp, notifications) = notifications().await;
    let notify =
        |kind, message: String| notifications.notify(&Nobody, About::default(), kind, message, &[]);
    notify(AlertKind::SpaceRemoved, "keep unread".into()).await;
    for i in 0..RETAINED + 5 {
        notify(AlertKind::AgentExited, format!("exit {i}")).await;
    }
    notifications
        .store
        .run(|db| {
            db.execute("UPDATE notifications SET read=1 WHERE id>1", [])?;
            Ok(())
        })
        .await
        .unwrap();
    notify(AlertKind::AgentExited, "final".into()).await;
    // Read rows beyond the newest RETAINED go; the older unread one stays.
    let all = notifications.list(false, u32::MAX).await.unwrap();
    assert_eq!(all.len(), RETAINED + 1);
    assert_eq!(all[0].message, "keep unread");
    assert_eq!(all[1].message, "exit 6");
    assert_eq!(all.iter().filter(|n| !n.read).count(), 2);
}
