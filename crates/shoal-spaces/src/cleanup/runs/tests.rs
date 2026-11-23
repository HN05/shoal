use crate::events::ShoalEventDetails;
use crate::events::ShoalEventKind;
use crate::stand_in::Fixture;
use shoal_protocol::space_names::SpaceName;

use crate::events::EventItem;

#[tokio::test]
async fn a_pass_is_recorded_after_removals_or_with_new_failures() {
    let manager = Fixture::new().await;
    manager.spaces.record_cleanup_run(None).await;
    manager
        .spaces
        .cleanup_removals
        .push(&SpaceName::parse("fix").unwrap());
    manager
        .spaces
        .cleanup_removals
        .push(&SpaceName::parse("chat").unwrap());
    manager.spaces.record_cleanup_run(None).await;
    // The removals were recorded with the pass before.
    manager
        .spaces
        .record_cleanup_run(Some("idle cleanup: denied".into()))
        .await;
    let runs: Vec<_> = (manager.space_events(None, 50).await.unwrap())
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
