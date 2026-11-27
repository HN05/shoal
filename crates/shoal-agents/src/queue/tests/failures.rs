use shoal_protocol::protocol::RetryStart;

use super::*;

/// Restoring a failed launch queues it again as it was, under its own
/// entry, and the queue then starts it.
#[tokio::test]
async fn a_restored_failed_launch_queues_again_as_it_was() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("work").await;
    let launch = QueuedLaunch {
        prompt: Some("Fix the login".into()),
        mode: Some("plan".into()),
        ..claude()
    };
    let request = queued_in(&workspace, launch);
    let entry = fixture.agents.queue_add(daemon, request).await;
    let entry = entry.unwrap().entry.unwrap();
    (fixture.agents.fail_queue_entry(daemon, &entry.id, "gone"))
        .await
        .unwrap();

    let retried = (fixture.agents)
        .retry_failed_launch(daemon, &entry.id, RetryStart::Queued)
        .await
        .unwrap();

    assert_eq!(retried.id, entry.id);
    assert_eq!(retried.mode, Some("plan".into()));
    dispatch(&fixture).await;
    let start = fixture.agents.queued_plan(&entry.id).await.unwrap().start;
    assert_eq!(start.launch.prompt.as_deref(), Some("Fix the login"));
    assert_eq!(start.launch.mode, Some("plan".into()));
}

/// A permission change a restore makes reaches a failed launch: the entry
/// the queue lists and the plan its agent starts under once restored.
#[tokio::test]
async fn a_failed_launch_takes_a_permission_change() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("work").await;
    let request = launch_in(&workspace, false);
    let entry = fixture.agents.queue_add(daemon, request).await;
    let entry = entry.unwrap().entry.unwrap();
    (fixture.agents.fail_queue_entry(daemon, &entry.id, "gone"))
        .await
        .unwrap();
    let change = PermissionChange::assigned("merge=direct").unwrap();
    let launches = [SpaceSelector::id(entry.space().unwrap())];
    let targets = PermissionTargets {
        agents: &[],
        launches: &launches,
    };

    (fixture
        .agents
        .change_stopped_permissions(daemon, targets, &change, None))
    .await
    .unwrap();

    let failed = fixture
        .agents
        .queue_entries(daemon)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(failed.narrowing, change.set);
    let retried = (fixture.agents)
        .retry_failed_launch(daemon, &entry.id, RetryStart::Queued)
        .await
        .unwrap();
    assert_eq!(retried.narrowing, change.set);
    dispatch(&fixture).await;
    let start = fixture.agents.queued_plan(&retried.id).await.unwrap().start;
    assert_eq!(start.launch.narrowing, change.set);
}

/// A failed launch restored now starts in the same request, past a full
/// machine, so no admission pass takes it between its requeue and start.
#[tokio::test]
async fn a_failed_launch_restored_now_starts_at_once() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let daemon = &*fixture.daemon;
    let entry = add(&fixture, "work").await.entry.unwrap();
    assert_eq!(entry.state, QueueState::Starting);
    let failure = "the place did not open";
    (fixture.agents.fail_queue_entry(daemon, &entry.id, failure))
        .await
        .unwrap();
    add(&fixture, "busy").await;

    let retried = (fixture.agents)
        .retry_failed_launch(daemon, &entry.id, RetryStart::Now)
        .await
        .unwrap();

    assert_eq!(
        (retried.id, retried.state),
        (entry.id, QueueState::Starting)
    );
}
