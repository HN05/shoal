use super::*;
use crate::{
    daemon::{
        test_support::manager,
        workspace::agent_queue::tests::{
            add, await_restore, bounded, connected_agent, create, limit_globally, register,
            request, started, stopped_agents,
        },
    },
    model::{Dependency, QueueRequest},
    protocol::QueuedLaunches,
};

/// The waiting entries' names in queue order.
async fn waiting(manager: &Manager) -> Vec<String> {
    manager
        .queue_entries()
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.state == QueueState::Queued)
        .map(|entry| entry.name)
        .collect()
}

#[tokio::test]
async fn a_stop_cancels_the_launches_waiting_to_open_its_workspace() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    add(&manager, &repo, "first").await;
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None, Default::default())
        .await
        .unwrap();
    let launch = manager
        .queue_add(QueueRequest {
            workspace: Some(child.name.clone()),
            ..request(&repo, "child", false)
        })
        .await
        .unwrap()
        .entry
        .unwrap();
    add(&manager, &repo, "later").await;
    assert_eq!(waiting(&manager).await, ["child", "later"]);

    // `shoal stop --all` stops the workspace; `shoal daemon stop` leaves it.
    let targets = async |launches| {
        manager
            .stop_targets(launches)
            .await
            .unwrap()
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        targets(QueuedLaunches::Cancel).await,
        std::slice::from_ref(&child.id)
    );
    assert!(targets(QueuedLaunches::Keep).await.is_empty());

    let kept = manager
        .stop_workspace_manually(&child.id, false, None, QueuedLaunches::Keep)
        .await
        .unwrap();
    assert!(kept.cancelled.is_empty());
    assert_eq!(waiting(&manager).await, ["child", "later"]);
    let cancelled = manager
        .stop_workspace_manually(&child.id, false, None, QueuedLaunches::Cancel)
        .await
        .unwrap();
    let cancelled: Vec<_> = cancelled
        .cancelled
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(cancelled, [launch.id]);
    // A launch of a new workspace stays queued.
    assert_eq!(waiting(&manager).await, ["later"]);
}

#[tokio::test]
async fn moved_entries_keep_their_new_place_and_start_in_it() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let first = add(&manager, &repo, "first").await.entry.unwrap();
    for name in ["a", "b", "c"] {
        add(&manager, &repo, name).await;
    }
    let moved = manager
        .queue_move("c", QueuePlace::Position(1), None)
        .await
        .unwrap();
    assert_eq!((moved.entry.position, moved.stopped), (Some(1), false));
    assert_eq!(waiting(&manager).await, ["c", "a", "b"]);
    manager
        .queue_move("c", QueuePlace::Last, None)
        .await
        .unwrap();
    assert_eq!(waiting(&manager).await, ["a", "b", "c"]);
    let c = manager.selected_entry("c", None).await.unwrap();
    let moved = manager
        .queue_move("b", QueuePlace::Before(c.id), None)
        .await
        .unwrap();
    assert_eq!(moved.entry.position, Some(2));
    manager
        .queue_move("a", QueuePlace::Position(3), None)
        .await
        .unwrap();
    assert_eq!(waiting(&manager).await, ["b", "c", "a"]);
    // A starting entry has no place to move from.
    manager
        .queue_move("first", QueuePlace::Last, None)
        .await
        .unwrap_err();

    manager.fail_queue_entry(&first.id, "gone").await.unwrap();
    manager.dispatch_queue().await.unwrap();
    let b = manager.selected_entry("b", None).await.unwrap();
    assert!(started(&b.id));
    assert_eq!(waiting(&manager).await, ["c", "a"]);
}

#[tokio::test]
async fn entries_start_now_past_the_limits_unless_they_wait_for_work() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    add(&manager, &repo, "first").await;
    add(&manager, &repo, "a").await;
    let dependent = manager
        .queue_add(QueueRequest {
            after: vec![Dependency::Work("a".into())],
            ..request(&repo, "dependent", false)
        })
        .await
        .unwrap();
    add(&manager, &repo, "b").await;

    let b = manager.queue_start("b").await.unwrap();
    assert_eq!(b.state, QueueState::Starting);
    assert!(started(&b.id));
    assert_eq!(waiting(&manager).await, ["a", "dependent"]);
    manager.queue_start("dependent").await.unwrap_err();
    assert!(!started(&dependent.entry.unwrap().id));
    manager.queue_start("first").await.unwrap_err();
}

#[tokio::test]
async fn a_stopped_agent_restores_now_once_its_processes_stopped() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "stopped").await;
    let agents = stopped_agents(&manager, &[&workspace]).await;
    manager.queue_start("stopped").await.unwrap_err();

    // Without healthy readings it waits until started.
    let restoring = await_restore(&manager, &agents[0].0).await;
    manager.dispatch_queue().await.unwrap();
    assert_eq!(waiting(&manager).await, ["stopped"]);
    manager.queue_start("stopped").await.unwrap();
    assert_eq!(bounded(restoring).await.unwrap().unwrap(), Grant::Queue);
}

#[tokio::test]
async fn cancelling_a_restore_leaves_its_agent_stopped() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "stopped").await;
    let mut agents = stopped_agents(&manager, &[&workspace]).await;
    let (execution, stop) = &mut agents[0];
    stop.borrow_and_update();

    manager.queue_remove("stopped", None).await.unwrap();
    assert!(manager.queue_entries().await.unwrap().is_empty());
    // Its wrapper stops waiting and keeps the recovery record.
    assert!(stop.has_changed().unwrap());
    assert!(!manager.agent_was_overloaded(execution).await);
    assert!(manager.executions.stop_request(execution).await.is_some());
    assert!(
        crate::execution::recovery::record_path(&manager.paths, &workspace.id, execution).is_file()
    );
}

#[tokio::test]
async fn a_restore_whose_wrapper_must_reattach_is_not_cancelled() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "stopped").await;
    let agents = stopped_agents(&manager, &[&workspace]).await;
    manager.detach_execution(&agents[0].0).await;
    manager.queue_remove("stopped", None).await.unwrap_err();
    assert_eq!(waiting(&manager).await, ["stopped"]);
}

#[tokio::test]
async fn cancelling_succeeds_when_the_stopped_wrapper_removes_its_entry_first() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "stopped").await;
    let mut agents = stopped_agents(&manager, &[&workspace]).await;
    let (execution, mut stop) = agents.remove(0);
    stop.borrow_and_update();
    // The wrapper finishes as soon as it is stopped, as its connection does.
    let finished = {
        let manager = manager.clone();
        tokio::spawn(async move {
            stop.changed().await.unwrap();
            manager.remove_restore(&execution).await;
        })
    };
    manager.queue_remove("stopped", None).await.unwrap();
    bounded(finished).await.unwrap();
    assert!(manager.queue_entries().await.unwrap().is_empty());
}

#[tokio::test]
async fn workspace_processes_cancel_and_move_back_only_entries_below_theirs() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    add(&manager, &repo, "first").await;
    let parent = create(&manager, &repo, "parent").await;
    let caller = Caller {
        space_id: parent.id.clone(),
        execution: None,
    };
    let mut children = Vec::new();
    for name in ["one", "two"] {
        let child = manager
            .split_workspace(&parent.id, name.into(), None, Default::default())
            .await
            .unwrap();
        manager
            .queue_add(QueueRequest {
                workspace: Some(child.id.clone()),
                ..request(&repo, name, false)
            })
            .await
            .unwrap();
        children.push(child);
    }
    add(&manager, &repo, "other").await;
    let caller = Some(&caller);

    let visible = manager.visible_queue(caller).await.unwrap();
    let names: Vec<_> = visible.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["one", "two"]);
    manager.queue_remove("other", caller).await.unwrap_err();
    manager
        .queue_move("two", QueuePlace::Position(1), caller)
        .await
        .unwrap_err();
    manager
        .queue_move("one", QueuePlace::Last, caller)
        .await
        .unwrap();
    assert_eq!(waiting(&manager).await, ["two", "other", "one"]);
    manager.queue_remove("one", caller).await.unwrap();
    assert_eq!(waiting(&manager).await, ["two", "other"]);

    // Starting an entry now is for unscoped callers.
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: parent.id.clone(),
                execution: None,
            },
        )
        .await;
    let mut method = crate::protocol::QueueMethod::QueueStart {
        entry: "two".into(),
    };
    assert!(
        crate::daemon::scope::authorize(&manager, Some("scope"), &mut method)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_moved_running_agent_restores_after_the_entries_ahead_of_it() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let api = create(&manager, &repo, "api").await;
    let (execution, mut stop) = connected_agent(&manager, &api).await;
    let next = add(&manager, &repo, "next").await.entry.unwrap();

    let moved = manager
        .queue_move("api", QueuePlace::Last, None)
        .await
        .unwrap();
    assert!(moved.stopped);
    assert_eq!(
        (moved.entry.kind, moved.entry.position),
        (QueueKind::Restore, Some(2))
    );
    assert!(stop.has_changed().unwrap() && *stop.borrow_and_update());
    assert_eq!(waiting(&manager).await, ["next", "api"]);
    // Until its processes are proven stopped the agent keeps its slot.
    manager.dispatch_queue().await.unwrap();
    assert!(!started(&next.id));

    // Its wrapper queues the restore again as it waits, which keeps its place.
    manager.queue_agent_restore(&execution).await.unwrap();
    let restoring = await_restore(&manager, &execution).await;
    manager.dispatch_queue().await.unwrap();
    assert!(started(&next.id));
    assert_eq!(waiting(&manager).await, ["api"]);
    assert!(!manager.queue_entries().await.unwrap()[1].waits_for_update);
    // It needs only a free slot, without healthy readings.
    manager.fail_queue_entry(&next.id, "gone").await.unwrap();
    manager.dispatch_queue().await.unwrap();
    assert_eq!(bounded(restoring).await.unwrap().unwrap(), Grant::Queue);
}

#[tokio::test]
async fn only_restorable_agents_move_and_only_for_unscoped_callers() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let api = create(&manager, &repo, "api").await;
    let (execution, stop) = connected_agent(&manager, &api).await;
    // A place outside the queue stops nothing.
    manager
        .queue_move("api", QueuePlace::Position(2), None)
        .await
        .unwrap_err();
    assert!(!stop.has_changed().unwrap());
    assert!(!manager.agent_was_overloaded(&execution).await);
    assert!(manager.queue_entries().await.unwrap().is_empty());
    let caller = Caller {
        space_id: create(&manager, &repo, "parent").await.id,
        execution: None,
    };
    manager
        .queue_move("api", QueuePlace::Last, Some(&caller))
        .await
        .unwrap_err();
    manager
        .queue_move("parent", QueuePlace::Last, None)
        .await
        .unwrap_err();

    manager
        .track_agent(
            &execution,
            &crate::execution::TrackedAgent::named("custom"),
            &api.id,
            &api.name,
            false,
            std::time::Duration::ZERO,
        )
        .await;
    manager
        .queue_move("api", QueuePlace::Last, None)
        .await
        .unwrap_err();
    assert!(manager.queue_entries().await.unwrap().is_empty());
}
