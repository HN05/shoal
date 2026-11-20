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
/// whether its wrapper or, once that is gone, the daemon restores it.
#[tokio::test]
async fn an_agent_waiting_for_its_restore_refuses_another_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "asleep").await;
    let (execution, _stop) = connected_agent(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("critical memory pressure")
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
    let refused = refusal(queue_worker(&manager, &workspace).await);
    assert!(refused.contains("shoal queue"), "{refused}");
    let entries = manager.queue_entries().await.unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
}

#[tokio::test]
async fn a_stopped_agent_refuses_another_worker_until_it_is_resumed_or_forgotten() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "stopped").await;
    let execution = ExecutionId::from("stopped-agent".to_owned());
    let record = crate::execution::recovery::save_record(
        &manager.paths,
        &workspace.id,
        &execution,
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    let refused = refusal(queue_worker(&manager, &workspace).await);
    assert!(
        refused.contains("shoal restore --discard-agent"),
        "{refused}"
    );
    std::fs::remove_file(record).unwrap();
    queue_worker(&manager, &workspace).await.unwrap();
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
