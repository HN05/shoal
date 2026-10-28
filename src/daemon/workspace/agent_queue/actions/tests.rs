use super::*;
use crate::{
    daemon::workspace::agent_queue::tests::{
        add, await_restore, bounded, create, limit_globally, register, request, started,
        stopped_agents,
    },
    model::{Dependency, QueueRequest},
    test_support::manager,
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
    assert_eq!(moved.position, Some(1));
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
    assert_eq!(moved.position, Some(2));
    manager
        .queue_move("a", QueuePlace::Position(3), None)
        .await
        .unwrap();
    assert_eq!(waiting(&manager).await, ["b", "c", "a"]);
    // A starting entry has no place to move from.
    let error = manager
        .queue_move("first", QueuePlace::Last, None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("already starting"));

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
    let error = manager.queue_start("dependent").await.unwrap_err();
    assert!(format!("{error:#}").contains("waits for a"), "{error:#}");
    assert!(!started(&dependent.entry.unwrap().id));
    let error = manager.queue_start("first").await.unwrap_err();
    assert!(format!("{error:#}").contains("already starting"));
}

#[tokio::test]
async fn a_stopped_agent_restores_now_once_its_processes_stopped() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "stopped").await;
    let agents = stopped_agents(&manager, &[&workspace]).await;
    let error = manager.queue_start("stopped").await.unwrap_err();
    assert!(format!("{error:#}").contains("still stopping"));

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
    let request = manager.executions.stop_request(execution).await.unwrap();
    assert!(request.reason.contains("shoal queue cancel"));
    assert!(
        crate::execution::recovery::record_path(&manager.paths, &workspace.id, execution).is_file()
    );
}

#[tokio::test]
async fn workspace_processes_cancel_and_move_back_only_entries_below_theirs() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    add(&manager, &repo, "first").await;
    let parent = create(&manager, &repo, "parent").await;
    let caller = Caller {
        workspace_id: parent.id.clone(),
        execution: None,
    };
    let mut children = Vec::new();
    for name in ["one", "two"] {
        let child = manager
            .split_workspace(&parent.id, name.into(), None)
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
    let error = manager.queue_remove("other", caller).await.unwrap_err();
    assert!(format!("{error:#}").contains("not in the agent queue"));
    let error = manager
        .queue_move("two", QueuePlace::Position(1), caller)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("only move queue entries back"));
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
                workspace_id: parent.id.clone(),
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
