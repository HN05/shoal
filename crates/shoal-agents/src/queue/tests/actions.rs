use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::progress::Blocker;

use super::*;

/// The waiting entries' names in queue order.
async fn waiting(fixture: &Fixture) -> Vec<String> {
    (fixture.agents.queue_entries(&*fixture.daemon))
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.state == QueueState::Queued)
        .map(|entry| entry.name.into())
        .collect()
}

/// Start the entry `name` now, or `here` in the caller's terminal.
async fn start(fixture: &Fixture, name: &str, here: bool) -> Result<QueueEntry> {
    let entry = queued(fixture, name).await;
    (fixture.agents.queue_start(&*fixture.daemon, &entry, here)).await
}

/// Cancel the waiting entry `name`.
async fn remove(fixture: &Fixture, name: &str) -> Result<QueueEntry> {
    let entry = queued(fixture, name).await;
    (fixture.agents.queue_remove(&*fixture.daemon, &entry, None)).await
}

#[tokio::test]
async fn entries_start_now_past_the_limits_unless_they_wait_for_work() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    add(&fixture, "first").await;
    add(&fixture, "a").await;
    let a = queued(&fixture, "a").await;
    let dependent = QueueRequest {
        blockers: vec![Blocker::Space(a)],
        ..request(&fixture, "dependent", false).await
    };
    let dependent = fixture.agents.queue_add(&*fixture.daemon, dependent).await;
    add(&fixture, "b").await;

    let b = start(&fixture, "b", false).await.unwrap();
    assert_eq!(b.state, QueueState::Starting);
    assert!(started(&b.id));
    assert_eq!(waiting(&fixture).await, ["a", "dependent"]);
    start(&fixture, "dependent", false).await.unwrap_err();
    assert!(!started(&dependent.unwrap().entry.unwrap().id));
    start(&fixture, "first", false).await.unwrap_err();
}

#[tokio::test]
async fn a_launch_started_here_is_left_to_the_caller() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    add(&fixture, "first").await;
    add(&fixture, "here").await;

    let here = start(&fixture, "here", true).await.unwrap();
    assert_eq!(here.state, QueueState::Starting);
    assert!(!started(&here.id));
    assert!(waiting(&fixture).await.is_empty());
}

#[tokio::test]
async fn only_a_launch_starts_here() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("stopped").await;
    let _agents = stopped_agents(&fixture, &[&workspace]).await;
    start(&fixture, "stopped", true).await.unwrap_err();
    assert_eq!(waiting(&fixture).await, ["stopped"]);
}

#[tokio::test]
async fn a_held_waiting_entry_waits_for_its_holder() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let first = add(&fixture, "first").await.entry.unwrap();
    add(&fixture, "held").await;
    add(&fixture, "after").await;
    let held = QueueEntryId::from(queued(&fixture, "held").await);
    fixture.agents.hold_queue_entry(&held).await.unwrap();

    (fixture
        .agents
        .fail_queue_entry(&*fixture.daemon, &first.id, "gone"))
    .await
    .unwrap();
    dispatch(&fixture).await;
    assert!(!started(&held));
    assert!(started(&queued(&fixture, "after").await.into()));
    fixture.agents.release_queue_hold(&held).await.unwrap();
    limit_globally(&fixture, 2);
    dispatch(&fixture).await;
    assert!(started(&held));
}

#[tokio::test]
async fn a_stopped_agent_restores_now_once_its_processes_stopped() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("stopped").await;
    let agents = stopped_agents(&fixture, &[&workspace]).await;
    start(&fixture, "stopped", false).await.unwrap_err();

    // Without healthy readings it waits until started.
    let restoring = await_restore(&fixture, &agents[0].0).await;
    dispatch(&fixture).await;
    assert_eq!(waiting(&fixture).await, ["stopped"]);
    start(&fixture, "stopped", false).await.unwrap();
    assert_eq!(bounded(restoring).await.unwrap().unwrap(), Grant::Queue);
}

#[tokio::test]
async fn a_wake_from_the_agents_place_starts_its_restore_once_it_waits() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("stopped").await;
    let agents = stopped_agents(&fixture, &[&workspace]).await;
    let execution = &agents[0].0;
    let wake = || {
        fixture
            .agents
            .wake_waiting_restore(&*fixture.daemon, execution)
    };
    // Still stopping: the wake waits for the restore to wait.
    assert!(!wake().await.unwrap());
    assert_eq!(waiting(&fixture).await, ["stopped"]);

    let restoring = await_restore(&fixture, execution).await;
    dispatch(&fixture).await;
    assert_eq!(waiting(&fixture).await, ["stopped"]);
    assert!(wake().await.unwrap());
    assert_eq!(bounded(restoring).await.unwrap().unwrap(), Grant::Queue);
}

#[tokio::test]
async fn cancelling_a_restore_leaves_its_agent_stopped() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("stopped").await;
    let mut agents = stopped_agents(&fixture, &[&workspace]).await;
    let (execution, stop) = &mut agents[0];
    stop.borrow_and_update();

    remove(&fixture, "stopped").await.unwrap();
    assert!(live(&fixture).await.is_empty());
    // Its wrapper stops waiting and keeps the recovery record.
    assert!(stop.has_changed().unwrap());
    assert!(!fixture.agents.agent_was_overloaded(execution).await);
    let request = fixture.agents.executions().stop_request(execution).await;
    assert!(request.is_some());
    let paths = &fixture.agents.paths;
    assert!(crate::execution::recovery::record_path(paths, &workspace.id, execution).is_file());
}

#[tokio::test]
async fn a_restore_whose_wrapper_must_reattach_is_not_cancelled() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("stopped").await;
    let agents = stopped_agents(&fixture, &[&workspace]).await;
    fixture.agents.detach_execution(&agents[0].0).await;
    remove(&fixture, "stopped").await.unwrap_err();
    assert_eq!(waiting(&fixture).await, ["stopped"]);
}

#[tokio::test]
async fn cancelling_succeeds_when_the_stopped_wrapper_removes_its_entry_first() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("stopped").await;
    let mut agents = stopped_agents(&fixture, &[&workspace]).await;
    let (execution, mut stop) = agents.remove(0);
    stop.borrow_and_update();
    // The wrapper finishes as soon as it is stopped, as its connection does.
    let finished = {
        let agents = fixture.agents.clone();
        tokio::spawn(async move {
            stop.changed().await.unwrap();
            agents.remove_restore(&execution).await;
        })
    };
    remove(&fixture, "stopped").await.unwrap();
    bounded(finished).await.unwrap();
    assert!(live(&fixture).await.is_empty());
}
