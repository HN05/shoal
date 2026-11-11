use crate::client::execution_ids::ExecutionId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        test_support::{global_config, manager},
        workspace::{
            Manager,
            agent_queue::tests::{connected_agent, create, register, request},
        },
    },
    model::{QueueRequest, Repository, Workspace},
};

/// Queue a launch of the agent in `workspace`, as a worker start does.
async fn launch_in(
    manager: &Manager,
    repository: &Repository,
    workspace: &Workspace,
) -> anyhow::Result<()> {
    manager
        .queue_add(QueueRequest {
            workspace: Some(SpaceSelector::id(&workspace.id)),
            ..request(repository, workspace.name.as_str(), false)
        })
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
    let refused = refusal(launch_in(&manager, &repo, &workspace).await);
    assert!(refused.contains("shoal attach"), "{refused}");
    assert!(manager.queue_entries().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_queued_agent_refuses_another_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_agents = 'none'"));
    let workspace = create(&manager, &repo, "waiting").await;
    launch_in(&manager, &repo, &workspace).await.unwrap();
    let refused = refusal(launch_in(&manager, &repo, &workspace).await);
    assert!(refused.contains("shoal queue"), "{refused}");
    assert_eq!(manager.queue_entries().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_stopped_agent_refuses_another_worker_until_it_is_resumed_or_discarded() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "stopped").await;
    let execution = ExecutionId::from("stopped-agent".to_owned());
    let record = crate::execution::recovery::save_record(
        &manager.paths,
        &workspace.id,
        &execution,
        &crate::execution::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    let refused = refusal(launch_in(&manager, &repo, &workspace).await);
    assert!(refused.contains("shoal resume --discard"), "{refused}");
    std::fs::remove_file(record).unwrap();
    launch_in(&manager, &repo, &workspace).await.unwrap();
}

#[tokio::test]
async fn a_workspace_without_an_agent_takes_a_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "idle").await;
    launch_in(&manager, &repo, &workspace).await.unwrap();
}
