use super::*;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        test_support::manager,
        workspace::agent_queue::tests::{
            add, await_restore, bounded, create, limit_globally, register, request, started,
            stopped_agents,
        },
    },
    model::{Blocker, QueueRequest},
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
        .map(|entry| entry.name.into())
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
        .split_workspace(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let launch = manager
        .queue_add(QueueRequest {
            workspace: Some(SpaceSelector::id(&child.id)),
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
        .stop_workspace_manually(
            &SpaceSelector::id(&child.id),
            false,
            None,
            QueuedLaunches::Keep,
        )
        .await
        .unwrap();
    assert!(kept.cancelled.is_empty());
    assert_eq!(waiting(&manager).await, ["child", "later"]);
    let cancelled = manager
        .stop_workspace_manually(
            &SpaceSelector::id(&child.id),
            false,
            None,
            QueuedLaunches::Cancel,
        )
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
async fn entries_start_now_past_the_limits_unless_they_wait_for_work() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    add(&manager, &repo, "first").await;
    add(&manager, &repo, "a").await;
    let a = manager.queued("a").await;
    let dependent = manager
        .queue_add(QueueRequest {
            blockers: vec![Blocker::Space(a)],
            ..request(&repo, "dependent", false)
        })
        .await
        .unwrap();
    add(&manager, &repo, "b").await;

    let b = manager
        .queue_start(&manager.queued("b").await)
        .await
        .unwrap();
    assert_eq!(b.state, QueueState::Starting);
    assert!(started(&b.id));
    assert_eq!(waiting(&manager).await, ["a", "dependent"]);
    manager
        .queue_start(&manager.queued("dependent").await)
        .await
        .unwrap_err();
    assert!(!started(&dependent.entry.unwrap().id));
    manager
        .queue_start(&manager.queued("first").await)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn a_stopped_agent_restores_now_once_its_processes_stopped() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "stopped").await;
    let agents = stopped_agents(&manager, &[&workspace]).await;
    manager
        .queue_start(&manager.queued("stopped").await)
        .await
        .unwrap_err();

    // Without healthy readings it waits until started.
    let restoring = await_restore(&manager, &agents[0].0).await;
    manager.dispatch_queue().await.unwrap();
    assert_eq!(waiting(&manager).await, ["stopped"]);
    manager
        .queue_start(&manager.queued("stopped").await)
        .await
        .unwrap();
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

    manager
        .queue_remove(&manager.queued("stopped").await, None)
        .await
        .unwrap();
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
    manager
        .queue_remove(&manager.queued("stopped").await, None)
        .await
        .unwrap_err();
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
    manager
        .queue_remove(&manager.queued("stopped").await, None)
        .await
        .unwrap();
    bounded(finished).await.unwrap();
    assert!(manager.queue_entries().await.unwrap().is_empty());
}

#[tokio::test]
async fn workspace_processes_see_the_whole_queue_and_act_only_below_theirs() {
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
            .split_workspace(
                &SpaceSelector::id(&parent.id),
                name.into(),
                None,
                Default::default(),
            )
            .await
            .unwrap();
        manager
            .queue_add(QueueRequest {
                workspace: Some(SpaceSelector::id(&child.id)),
                ..request(&repo, name, false)
            })
            .await
            .unwrap();
        children.push(child);
    }
    add(&manager, &repo, "other").await;
    let caller = Some(&caller);

    // Every caller sees every entry, and what holds each waiting one back.
    let visible = manager.visible_queue().await.unwrap();
    let names: Vec<_> = visible
        .iter()
        .map(|entry| (entry.name.as_str(), entry.held_by))
        .collect();
    let max_agents = Some(crate::model::HeldBy::MaxAgents);
    assert_eq!(
        names,
        [
            ("first", None),
            ("one", max_agents),
            ("two", max_agents),
            ("other", max_agents)
        ]
    );
    let reachable = manager.reachable_queue(caller).await.unwrap();
    let names: Vec<_> = reachable.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["one", "two"]);
    manager
        .queue_remove(&manager.queued("other").await, caller)
        .await
        .unwrap_err();
    manager
        .queue_remove(&manager.queued("one").await, caller)
        .await
        .unwrap();
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
        crate::daemon::scope::authorize(&manager, Some(&"scope".into()), &mut method)
            .await
            .is_err()
    );
}
