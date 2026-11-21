use crate::daemon::{notifications::AlertKind, test_support};

#[tokio::test]
async fn a_removal_that_leaves_trust_entries_alerts_the_user() {
    let (root, manager) = test_support::manager().await;
    let path = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.display().to_string().into(), None, None)
        .await
        .unwrap();
    // A config Shoal cannot read keeps the entries it holds.
    std::fs::write(manager.paths.home.join(".claude.json"), "not JSON").unwrap();

    manager.remove_repository(&repo.id).await.unwrap();
    let alerts = manager.notifications.list(false, 10).await.unwrap();
    let left = alerts
        .iter()
        .find(|alert| alert.kind == AlertKind::TrustLeft);
    let left = left.unwrap_or_else(|| panic!("no trust alert: {alerts:?}"));
    assert!(left.message.contains(".claude.json"), "{}", left.message);
}
