use crate::daemon::{notifications::AlertKind, test_support};

#[tokio::test]
async fn a_removal_that_leaves_trust_entries_alerts_the_user() {
    let (root, manager) = test_support::manager().await;
    let path = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.display().to_string().into(), None, None)
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

#[tokio::test]
async fn a_development_build_leaves_kept_locations_outside_its_home() {
    let (root, manager) = test_support::manager().await;
    let path = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.display().to_string().into(), None, None)
        .await
        .unwrap();
    let outside = tempfile::tempdir_in("/tmp").unwrap();
    let kept = std::collections::BTreeMap::from([(
        crate::env::CLAUDE_CONFIG_DIR.to_owned(),
        outside.path().display().to_string(),
    )]);
    manager
        .keep_repository_agent_environment(&repo.id, kept)
        .await
        .unwrap();
    let checkout = std::fs::canonicalize(&repo.path).unwrap();
    let projects = serde_json::json!({"projects": {
        checkout.to_str().unwrap(): {"hasTrustDialogAccepted": true}
    }});
    let config = outside.path().join(".claude.json");
    std::fs::write(&config, projects.to_string()).unwrap();

    manager.remove_repository(&repo.id).await.unwrap();
    let left = std::fs::read_to_string(&config).unwrap();
    let kept = left.contains(checkout.to_str().unwrap());
    assert_eq!(kept, !crate::build::changes_user_files(), "{left}");
}
