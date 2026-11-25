use crate::protocol::operations::QueuedLaunch;

use super::*;

/// The agent of a new workspace that its caller runs here, asked to start
/// `now` or not, queued on a machine whose one slot another launch holds,
/// waiting for the workspace's preparation, which then releases it.
async fn prepared_here(fixture: &Fixture, now: bool) -> QueueEntry {
    limit_globally(fixture, 1);
    assert!(add(fixture, "busy").await.entry.is_some());
    let workspace = fixture.workspace("child").await;
    let preparing = WorkspaceState::Preparing;
    fixture.set_state(&workspace.id, preparing, None).await;
    let launch = QueuedLaunch {
        now,
        here: true,
        ..QueuedLaunch::new("claude".parse().unwrap())
    };
    let daemon = &*fixture.daemon;
    let space = fixture.space(&workspace.id);
    let entry = (fixture.agents)
        .queue_start_agent(daemon, (&space, true), launch, (None, Vec::new()))
        .await
        .unwrap()
        .entry
        .unwrap();
    (fixture.agents.release_setup_waits(daemon, &workspace.id))
        .await
        .unwrap();
    entry
}

/// The state of the entry `id`.
async fn state(fixture: &Fixture, id: &QueueEntryId) -> QueueState {
    let selected = fixture
        .agents
        .selected_entry(&*fixture.daemon, id.as_str(), None);
    selected.await.unwrap().state
}

/// A launch its caller runs, which asked to start now, stays waiting for
/// the caller once its preparation releases it, and the caller's claim
/// starts it for the caller past the full machine, which the daemon does
/// not start itself.
#[tokio::test]
async fn a_launch_run_here_waits_for_its_callers_claim_to_start_now() {
    let fixture = Fixture::new().await;
    let entry = prepared_here(&fixture, true).await;
    dispatch(&fixture).await;
    assert_eq!(state(&fixture, &entry.id).await, QueueState::Queued);

    let claimed = fixture
        .agents
        .queue_claim(&*fixture.daemon, &entry.id)
        .await;
    assert_eq!(claimed.unwrap().state, QueueState::Starting);
    assert!(!started(&entry.id));
}

/// A claim of a launch that did not ask to start now waits for a slot,
/// leaving the launch to the queue, and once a slot is free admits it for
/// the caller rather than the daemon.
#[tokio::test]
async fn a_claim_starts_a_launch_run_here_only_within_the_limits() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let entry = prepared_here(&fixture, false).await;
    let claimed = fixture.agents.queue_claim(daemon, &entry.id).await;
    assert_eq!(claimed.unwrap().state, QueueState::Queued);

    let busy = (live(&fixture).await.into_iter())
        .find(|queued| queued.name == "busy")
        .unwrap();
    (fixture.agents.fail_queue_entry(daemon, &busy.id, "done"))
        .await
        .unwrap();
    fixture.agents.agent_queue().hold(&entry.id);
    let claimed = fixture.agents.queue_claim(daemon, &entry.id).await;
    assert_eq!(claimed.unwrap().state, QueueState::Starting);
    assert!(!started(&entry.id));
}

/// The worker of a ready workspace that its caller runs, queued on a full
/// machine, stays the caller's when a slot frees before the caller claims
/// it, rather than starting in the background too.
#[tokio::test]
async fn a_queued_worker_run_here_waits_for_its_callers_claim() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let daemon = &*fixture.daemon;
    let busy = add(&fixture, "busy").await.entry.unwrap();
    let workspace = fixture.workspace("worker").await;
    let here = QueueRequest {
        starts_in_caller: true,
        ..launch_in(&workspace, false)
    };
    let entry = fixture.agents.queue_add(daemon, here).await;
    let entry = entry.unwrap().entry.unwrap();
    assert_eq!(entry.state, QueueState::Queued);

    (fixture.agents.fail_queue_entry(daemon, &busy.id, "done"))
        .await
        .unwrap();
    dispatch(&fixture).await;
    assert_eq!(state(&fixture, &entry.id).await, QueueState::Queued);
    assert!(!started(&entry.id));
    let claimed = fixture.agents.queue_claim(daemon, &entry.id).await;
    assert_eq!(claimed.unwrap().state, QueueState::Starting);
}
