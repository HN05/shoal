use crate::daemon::scope::Access;
use crate::{
    daemon::{
        agent_service::tests::queue::{add, create, launch_in, limit_globally, register},
        test_support::manager,
    },
    protocol::QueuedLaunches,
};
use crate::{
    daemon::{manager::Manager, scope::Caller},
    state::QueueState,
};
use shoal_protocol::space_names::SpaceSelector;

/// The waiting entries' names in queue order.
async fn waiting(manager: &Manager) -> Vec<String> {
    manager
        .agents
        .queue_entries(manager)
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
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let launch = manager
        .agents
        .queue_add(&*manager, launch_in(&child, false))
        .await
        .unwrap()
        .entry
        .unwrap();
    let later = add(&manager, &repo, "later").await.entry.unwrap();
    assert_eq!(waiting(&manager).await, ["child", "later"]);

    // `shoal stop --all` stops the workspace; `shoal daemon stop` leaves it.
    let targets = async |launches| {
        manager
            .agents
            .stop_targets(&*manager, launches)
            .await
            .unwrap()
            .into_iter()
            .map(|space| space.id.clone())
            .collect::<Vec<_>>()
    };
    let cancelling = targets(QueuedLaunches::Cancel).await;
    assert_eq!(cancelling.len(), 2, "{cancelling:?}");
    assert!(
        cancelling.contains(&child.id)
            && later.opens.is_some_and(|later| cancelling.contains(&later))
    );
    assert!(targets(QueuedLaunches::Keep).await.is_empty());

    let kept = manager
        .agents
        .stop_space_manually(
            &*manager,
            &SpaceSelector::id(&child.id),
            false,
            QueuedLaunches::Keep,
        )
        .await
        .unwrap();
    assert!(kept.cancelled.is_empty());
    assert_eq!(waiting(&manager).await, ["child", "later"]);
    let cancelled = manager
        .agents
        .stop_space_manually(
            &*manager,
            &SpaceSelector::id(&child.id),
            false,
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
    // The launch in another workspace stays queued.
    assert_eq!(waiting(&manager).await, ["later"]);
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
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let mut children = Vec::new();
    for name in ["one", "two"] {
        let child = manager
            .spaces
            .create_child(
                &*manager,
                &SpaceSelector::id(&parent.id),
                name.into(),
                None,
                Default::default(),
            )
            .await
            .unwrap();
        manager
            .agents
            .queue_add(&*manager, launch_in(&child, false))
            .await
            .unwrap();
        children.push(child);
    }
    add(&manager, &repo, "other").await;
    let caller = Some(&caller);

    // Every caller sees every entry, and what holds each waiting one back.
    let visible = manager.coordination.visible_queue(&*manager).await.unwrap();
    let names: Vec<_> = visible
        .iter()
        .map(|entry| (entry.name.as_str(), entry.held_by))
        .collect();
    let max_heavy_agents = Some(crate::model::HeldBy::MaxHeavyAgents);
    assert_eq!(
        names,
        [
            ("first", None),
            ("one", max_heavy_agents),
            ("two", max_heavy_agents),
            ("other", max_heavy_agents)
        ]
    );
    let reachable = manager
        .coordination
        .reachable_queue(&*manager, caller)
        .await
        .unwrap();
    let names: Vec<_> = reachable.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["one", "two"]);
    manager
        .agents
        .queue_remove(&*manager, &manager.queued("other").await, caller)
        .await
        .unwrap_err();
    manager
        .agents
        .queue_remove(&*manager, &manager.queued("one").await, caller)
        .await
        .unwrap();
    assert_eq!(waiting(&manager).await, ["two", "other"]);

    // Starting an entry now is for unscoped callers.
    manager
        .agents
        .executions()
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: parent.id.clone(),
                execution: None,
                granted: None,
                access: Access::Full,
                vouched: None,
            },
        )
        .await;
    let mut method = crate::protocol::QueueMethod::QueueStart {
        entry: "two".into(),
        here: false,
    };
    assert!(
        crate::daemon::scope::authorize(&manager, Some(&"scope".into()), &mut method)
            .await
            .is_err()
    );
}
