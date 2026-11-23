use crate::{
    daemon::{
        agent_service::tests::queue::{connected_agent, create, launch_in, register},
        manager::Manager,
        test_support::{global_config, manager},
    },
    model::{QueueRequest, Workspace},
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

/// Queue a launch of the agent in `workspace`, as a worker start does.
async fn queue_worker(manager: &Manager, workspace: &Workspace) -> anyhow::Result<()> {
    manager
        .agents
        .queue_add(manager, launch_in(workspace, false))
        .await
        .map(drop)
}

/// The refusal of a second worker, which names the command that frees the
/// workspace.
fn refusal(result: anyhow::Result<()>) -> String {
    format!("{:#}", result.expect_err("a second worker was queued"))
}

#[tokio::test]
async fn a_running_agent_refuses_another_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "busy").await;
    let _agent = connected_agent(&manager, &workspace).await;
    let refused = refusal(queue_worker(&manager, &workspace).await);
    assert!(refused.contains("shoal attach"), "{refused}");
    assert!(
        manager
            .agents
            .queue_entries(&*manager,)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_queued_agent_refuses_another_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false",
    ));
    let workspace = create(&manager, &repo, "waiting").await;
    queue_worker(&manager, &workspace).await.unwrap();
    let refused = refusal(queue_worker(&manager, &workspace).await);
    assert!(refused.contains("shoal queue"), "{refused}");
    assert_eq!(
        manager
            .agents
            .queue_entries(&*manager,)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// An agent that waits for its restore holds its workspace's one agent,
/// whether its wrapper or, once that is gone, the daemon restores it, as
/// an asleep agent a launch may replace.
#[tokio::test]
async fn an_agent_waiting_for_its_restore_refuses_another_worker() {
    use crate::daemon::agent_queue::worker::HasStoppedAgent;
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "asleep").await;
    let (execution, _stop) = connected_agent(&manager, &workspace).await;
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, crate::daemon::overload::Pressure::Memory)
            .await
    );
    manager
        .agents
        .queue_agent_restore(&*manager, &execution)
        .await
        .unwrap();
    let refused = queue_worker(&manager, &workspace).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.is::<HasStoppedAgent>()),
        "{refused:?}"
    );

    // Its wrapper is gone, and the daemon takes over its restore.
    (manager.store)
        .run(move |db| {
            db.execute("DELETE FROM executions WHERE id=?1", [execution])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let entries = manager.agents.queue_entries(&*manager).await.unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let refused = queue_worker(&manager, &workspace).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.is::<HasStoppedAgent>()),
        "{refused:?}"
    );

    // Replacing it takes its restore out of the queue.
    let space = space_of(&manager, &workspace).await;
    manager
        .agents
        .replace_stopped_worker(&*manager, &space)
        .await
        .unwrap();
    assert!(
        manager
            .agents
            .queue_entries(&*manager,)
            .await
            .unwrap()
            .is_empty()
    );
    queue_worker(&manager, &workspace).await.unwrap();
}

/// An asleep agent whose wrapper waits to restore it runs no turn, so a
/// launch that replaces it stops that wrapper, takes its restore out of
/// the queue and discards it.
#[tokio::test]
async fn replacing_an_asleep_agent_stops_the_wrapper_waiting_for_it() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "asleep").await;
    let (execution, stop) = connected_agent(&manager, &workspace).await;
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, crate::daemon::overload::Pressure::Memory)
            .await
    );
    manager
        .agents
        .queue_agent_restore(&*manager, &execution)
        .await
        .unwrap();
    let wrapper = wrapper_ending_when_stopped(&manager, &execution, stop);

    let admission = manager
        .agents
        .queue_add(
            &*manager,
            QueueRequest {
                replace: true,
                ..launch_in(&workspace, false)
            },
        )
        .await
        .unwrap();

    wrapper.await.unwrap();
    let entries = manager.agents.queue_entries(&*manager).await.unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].id, admission.entry.unwrap().id);
    let records = crate::execution::recovery::agent_records(&manager.paths, &workspace.id);
    assert!(records.unwrap().is_empty());
}

/// The wrapper of `execution`, which waits to restore its agent after the
/// stop `stop` has seen, ending it once the next stop asks it to, as a real
/// wrapper does.
fn wrapper_ending_when_stopped(
    manager: &std::sync::Arc<Manager>,
    execution: &ExecutionId,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let (manager, execution) = (manager.clone(), execution.clone());
    stop.borrow_and_update();
    tokio::spawn(async move {
        stop.changed().await.unwrap();
        let kind = crate::daemon::executions::ExecutionKind::Command;
        manager
            .agents
            .finish_execution(&*manager, execution, kind, Some(0))
            .await
            .unwrap();
    })
}

/// Record a stopped agent of `workspace` awaiting `shoal resume`,
/// returning its record's path.
fn stopped_agent(manager: &Manager, workspace: &Workspace) -> std::path::PathBuf {
    let execution = ExecutionId::from("stopped-agent".to_owned());
    crate::execution::recovery::save_record(
        &manager.paths,
        &workspace.id,
        &execution,
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap()
}

/// The space record of `workspace`, which a replacing launch reads.
async fn space_of(manager: &Manager, workspace: &Workspace) -> crate::model::Space {
    manager
        .spaces
        .space(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_stopped_agent_refuses_another_worker_until_it_is_resumed_or_forgotten() {
    use crate::daemon::agent_queue::worker::HasStoppedAgent;
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "stopped").await;
    let record = stopped_agent(&manager, &workspace);
    let refused = queue_worker(&manager, &workspace).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.is::<HasStoppedAgent>()),
        "{refused:?}"
    );
    std::fs::remove_file(record).unwrap();
    queue_worker(&manager, &workspace).await.unwrap();
}

#[tokio::test]
async fn replacing_a_stopped_agent_discards_it_and_takes_a_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "stopped").await;
    let record = stopped_agent(&manager, &workspace);
    let space = space_of(&manager, &workspace).await;
    manager
        .agents
        .replace_stopped_worker(&*manager, &space)
        .await
        .unwrap();
    assert!(!record.exists(), "the stopped agent's record stayed");
    queue_worker(&manager, &workspace).await.unwrap();
}

/// A start that reopens a workspace, such as on its issue, queues its
/// agent through the start's request, which replaces as a worker's does.
#[tokio::test]
async fn a_start_s_agent_that_replaces_discards_the_stopped_agent() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "reopened").await;
    let record = stopped_agent(&manager, &workspace);
    let launch = crate::protocol::operations::QueuedLaunch {
        replace: true,
        ..crate::protocol::operations::QueuedLaunch::new("claude".parse().unwrap())
    };
    let space = space_of(&manager, &workspace).await;

    let admission = manager
        .agents
        .queue_start_agent(&*manager, (&space, false), launch, (None, Vec::new()))
        .await
        .unwrap();

    assert!(admission.entry.is_some());
    assert!(!record.exists(), "the stopped agent's record stayed");
}

/// A replacing launch the queue refuses for another reason, such as a
/// workspace no longer ready, keeps the stopped agent.
#[tokio::test]
async fn a_replacing_launch_the_queue_refuses_keeps_the_stopped_agent() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "failed").await;
    let record = stopped_agent(&manager, &workspace);
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute("UPDATE workspaces SET state='failed' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();

    let refused = manager
        .agents
        .queue_add(
            &*manager,
            QueueRequest {
                replace: true,
                now: true,
                ..launch_in(&workspace, false)
            },
        )
        .await;

    assert!(
        refused.is_err(),
        "a launch in a failed workspace was queued"
    );
    assert!(record.exists());
}

#[tokio::test]
async fn replacing_refuses_a_running_agent() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "busy").await;
    let _agent = connected_agent(&manager, &workspace).await;
    let space = space_of(&manager, &workspace).await;
    let refused = manager
        .agents
        .replace_stopped_worker(&*manager, &space)
        .await;
    let refused = format!("{:#}", refused.expect_err("a running agent was replaced"));
    assert!(refused.contains("shoal stop"), "{refused}");
}

#[tokio::test]
async fn a_workspace_without_an_agent_takes_a_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "idle").await;
    queue_worker(&manager, &workspace).await.unwrap();
}

#[tokio::test]
async fn an_agent_without_a_launch_waits_out_one_queued_in_its_workspace() {
    use crate::daemon::agent_queue::worker::HasAgent;
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "worked").await;
    let blocker = create(&manager, &repo, "blocker").await;
    let selector = SpaceSelector::id(&workspace.id);
    // Nothing waits there, so a restored agent may register.
    drop(
        manager
            .agents
            .reserve_worker(&*manager, &selector)
            .await
            .unwrap(),
    );
    // A launch waits there for another space's work.
    let admission = manager
        .agents
        .queue_add(
            &*manager,
            QueueRequest {
                blockers: vec![shoal_protocol::progress::Blocker::Space(
                    blocker.id.to_string(),
                )],
                ..launch_in(&workspace, false)
            },
        )
        .await
        .unwrap();
    let entry = admission.entry.unwrap();
    let refused = manager
        .agents
        .reserve_worker(&*manager, &selector)
        .await
        .map(drop);
    assert!(
        refused.as_ref().is_err_and(|error| error.is::<HasAgent>()),
        "{refused:?}"
    );
    // Once it leaves the queue, the restore may go ahead.
    manager
        .agents
        .queue_remove(&*manager, entry.id.as_str(), None)
        .await
        .unwrap();
    drop(
        manager
            .agents
            .reserve_worker(&*manager, &selector)
            .await
            .unwrap(),
    );
}
