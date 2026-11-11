use super::*;
use crate::daemon::{events::EventItem, test_support::manager};

#[tokio::test]
async fn a_pass_is_recorded_after_removals_or_with_new_failures() {
    let (_temp, manager) = manager().await;
    manager.record_cleanup_run(None).await;
    manager
        .cleanup_removals
        .push(&SpaceName::parse("fix").unwrap());
    manager
        .cleanup_removals
        .push(&SpaceName::parse("chat").unwrap());
    manager.record_cleanup_run(None).await;
    // The removals were recorded with the pass before.
    manager
        .record_cleanup_run(Some("idle cleanup: denied".into()))
        .await;
    let runs: Vec<_> = (manager.workspace_events(None, 50).await.unwrap())
        .into_iter()
        .map(|item| match item {
            EventItem::Shoal(event) => event.details,
            other => panic!("unexpected event {other:?}"),
        })
        .collect();
    let mut removed = ShoalEventDetails::new(ShoalEventKind::CleanupRan);
    removed.removed = vec![
        SpaceName::parse("fix").unwrap(),
        SpaceName::parse("chat").unwrap(),
    ];
    let mut failed = ShoalEventDetails::new(ShoalEventKind::CleanupRan);
    failed.error = Some("idle cleanup: denied".into());
    assert_eq!(runs, [removed, failed]);
}
