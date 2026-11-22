use crate::daemon::agent_service::tests::queue::{
    add, launch_in, limit_globally, preparing, register, started,
};
use crate::{
    daemon::{manager::Manager, test_support::manager},
    model::{QueueEntry, QueueRequest, Repository},
    protocol::operations::QueuedLaunch,
    state::QueueState,
};

/// The agent of a new workspace of `repository` that its caller runs
/// here, asked to start `now` or not, queued on a machine whose one slot
/// another launch holds, waiting for the workspace's preparation, which
/// then releases it.
async fn prepared_here(manager: &Manager, repository: &Repository, now: bool) -> QueueEntry {
    limit_globally(manager, 1);
    assert!(add(manager, repository, "busy").await.entry.is_some());
    let workspace = preparing(manager, repository, "child").await;
    let launch = QueuedLaunch {
        now,
        here: true,
        ..QueuedLaunch::new("claude".parse().unwrap())
    };
    let entry = manager
        .agents
        .queue_start_agent(
            manager,
            (&workspace.clone().into(), true),
            launch,
            (None, Vec::new()),
        )
        .await
        .unwrap()
        .entry
        .unwrap();
    manager
        .agents
        .release_setup_waits(manager, &workspace.id)
        .await
        .unwrap();
    entry
}

/// A launch its caller runs, which asked to start now, stays waiting for
/// the caller once its preparation releases it, and the caller's claim
/// starts it for the caller past the full machine, which the daemon does
/// not start itself.
#[tokio::test]
async fn a_launch_run_here_waits_for_its_callers_claim_to_start_now() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let entry = prepared_here(&manager, &repo, true).await;
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let waiting = manager
        .agents
        .selected_entry(&*manager, entry.id.as_str(), None)
        .await
        .unwrap();
    assert_eq!(waiting.state, QueueState::Queued);

    let claimed = manager
        .agents
        .queue_claim(&*manager, &entry.id)
        .await
        .unwrap();
    assert_eq!(claimed.state, QueueState::Starting);
    assert!(!started(&entry.id));
}

/// A claim of a launch that did not ask to start now waits for a slot,
/// leaving the launch to the queue, and once a slot is free admits it for
/// the caller rather than the daemon.
#[tokio::test]
async fn a_claim_starts_a_launch_run_here_only_within_the_limits() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let entry = prepared_here(&manager, &repo, false).await;
    let claimed = manager
        .agents
        .queue_claim(&*manager, &entry.id)
        .await
        .unwrap();
    assert_eq!(claimed.state, QueueState::Queued);

    let busy = (manager
        .agents
        .queue_entries(&*manager)
        .await
        .unwrap()
        .into_iter())
    .find(|queued| queued.name == "busy")
    .unwrap();
    manager
        .agents
        .fail_queue_entry(&*manager, &busy.id, "done")
        .await
        .unwrap();
    manager.agents.agent_queue().hold(&entry.id);
    let claimed = manager
        .agents
        .queue_claim(&*manager, &entry.id)
        .await
        .unwrap();
    assert_eq!(claimed.state, QueueState::Starting);
    assert!(!started(&entry.id));
}

/// The worker of a ready workspace that its caller runs, queued on a full
/// machine, stays the caller's when a slot frees before the caller claims
/// it, rather than starting in the background too.
#[tokio::test]
async fn a_queued_worker_run_here_waits_for_its_callers_claim() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let busy = add(&manager, &repo, "busy").await.entry.unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let here = QueueRequest {
        starts_in_caller: true,
        ..launch_in(&workspace, false)
    };
    let entry = manager
        .agents
        .queue_add(&*manager, here)
        .await
        .unwrap()
        .entry
        .unwrap();
    assert_eq!(entry.state, QueueState::Queued);

    manager
        .agents
        .fail_queue_entry(&*manager, &busy.id, "done")
        .await
        .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let waiting = manager
        .agents
        .selected_entry(&*manager, entry.id.as_str(), None)
        .await
        .unwrap();
    assert_eq!(waiting.state, QueueState::Queued);
    assert!(!started(&entry.id));
    let claimed = manager
        .agents
        .queue_claim(&*manager, &entry.id)
        .await
        .unwrap();
    assert_eq!(claimed.state, QueueState::Starting);
}
