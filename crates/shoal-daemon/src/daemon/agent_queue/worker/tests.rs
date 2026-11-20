use crate::{
    daemon::{
        agent_queue::tests::{connected_agent, create, launch_in, register},
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
        .queue_add(launch_in(workspace, false))
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
    assert!(manager.queue_entries().await.unwrap().is_empty());
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
    assert_eq!(manager.queue_entries().await.unwrap().len(), 1);
}

/// An agent that waits for its restore holds its workspace's one agent,
/// whether its wrapper or, once that is gone, the daemon restores it, as
/// an asleep agent a launch may replace.
#[tokio::test]
async fn an_agent_waiting_for_its_restore_refuses_another_worker() {
    use super::StoppedAgent;
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "asleep").await;
    let (execution, _stop) = connected_agent(&manager, &workspace).await;
    assert!(
        manager
            .stop_for_overload(crate::daemon::overload::Pressure::Memory)
            .await
    );
    manager.queue_agent_restore(&execution).await.unwrap();
    refusal(queue_worker(&manager, &workspace).await);

    // Its wrapper is gone, and the daemon takes over its restore.
    (manager.store)
        .run(move |db| {
            db.execute("DELETE FROM executions WHERE id=?1", [execution])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    let entries = manager.queue_entries().await.unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let refused = queue_worker(&manager, &workspace).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.is::<StoppedAgent>()),
        "{refused:?}"
    );

    // Replacing it takes its restore out of the queue.
    let space = space_of(&manager, &workspace).await;
    manager.replace_stopped_worker(&space).await.unwrap();
    assert!(manager.queue_entries().await.unwrap().is_empty());
    queue_worker(&manager, &workspace).await.unwrap();
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
        .space(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_stopped_agent_refuses_another_worker_until_it_is_resumed_or_forgotten() {
    use super::StoppedAgent;
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "stopped").await;
    let record = stopped_agent(&manager, &workspace);
    let refused = queue_worker(&manager, &workspace).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.is::<StoppedAgent>()),
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
    manager.replace_stopped_worker(&space).await.unwrap();
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
        .queue_start_agent((&space, false), launch, (None, Vec::new()))
        .await
        .unwrap();

    assert!(admission.entry.is_some());
    assert!(!record.exists(), "the stopped agent's record stayed");
}

#[tokio::test]
async fn replacing_refuses_a_running_agent() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "busy").await;
    let _agent = connected_agent(&manager, &workspace).await;
    let space = space_of(&manager, &workspace).await;
    let refused = manager.replace_stopped_worker(&space).await;
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
    use super::HasAgent;
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "worked").await;
    let blocker = create(&manager, &repo, "blocker").await;
    let selector = SpaceSelector::id(&workspace.id);
    // Nothing waits there, so a restored agent may register.
    drop(manager.reserve_worker(&selector).await.unwrap());
    // A launch waits there for another space's work.
    let admission = manager
        .queue_add(QueueRequest {
            blockers: vec![shoal_protocol::progress::Blocker::Space(
                blocker.id.to_string(),
            )],
            ..launch_in(&workspace, false)
        })
        .await
        .unwrap();
    let entry = admission.entry.unwrap();
    let refused = manager.reserve_worker(&selector).await.map(drop);
    assert!(
        refused.as_ref().is_err_and(|error| error.is::<HasAgent>()),
        "{refused:?}"
    );
    // Once it leaves the queue, the restore may go ahead.
    manager.queue_remove(entry.id.as_str(), None).await.unwrap();
    drop(manager.reserve_worker(&selector).await.unwrap());
}
