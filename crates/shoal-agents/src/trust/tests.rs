use std::collections::BTreeMap;

use super::*;
use crate::stand_in::Fixture;

#[tokio::test]
async fn a_removal_that_leaves_trust_entries_alerts_the_user() {
    let fixture = Fixture::new().await;
    let repo = fixture.repository();
    // A config Shoal cannot read keeps the entries it holds.
    std::fs::write(fixture.agents.paths.home.join(".claude.json"), "not JSON").unwrap();

    let removed = TrustChange::Removed(Vec::new());
    (fixture
        .agents
        .update_agent_trust(&*fixture.daemon, &repo, removed))
    .await;

    let notified = fixture.daemon.notified.lock().unwrap();
    let left = notified
        .iter()
        .find(|(kind, _)| *kind == AlertKind::TrustLeft);
    let (_, left) = left.unwrap_or_else(|| panic!("no trust alert: {notified:?}"));
    assert!(left.contains(".claude.json"), "{left}");
}

#[tokio::test]
async fn a_development_build_leaves_kept_locations_outside_its_home() {
    let fixture = Fixture::new().await;
    let repo = fixture.repository();
    let outside = tempfile::tempdir_in("/tmp").unwrap();
    let kept = BTreeMap::from([(
        shoal_core::env::CLAUDE_CONFIG_DIR.to_owned(),
        outside.path().display().to_string(),
    )]);
    (fixture.agents)
        .keep_repository_agent_environment(&*fixture.daemon, &repo.id, kept)
        .await
        .unwrap();
    let checkout = std::fs::canonicalize(&repo.path).unwrap();
    let projects = serde_json::json!({"projects": {
        checkout.to_str().unwrap(): {"hasTrustDialogAccepted": true}
    }});
    let config = outside.path().join(".claude.json");
    std::fs::write(&config, projects.to_string()).unwrap();

    let removed = TrustChange::Removed(Vec::new());
    (fixture
        .agents
        .update_agent_trust(&*fixture.daemon, &repo, removed))
    .await;
    let left = std::fs::read_to_string(&config).unwrap();
    let kept = left.contains(checkout.to_str().unwrap());
    assert_eq!(kept, !shoal_core::build::changes_user_files(), "{left}");
}
