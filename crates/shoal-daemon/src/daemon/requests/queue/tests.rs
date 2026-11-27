use crate::{
    daemon::{
        agent_service::tests::queue::launch_in,
        test_support::{global_config, manager, repository},
    },
    protocol::{QueueMethod, RetryStart},
};

/// A failed launch the user restores through the queue, not only in their
/// own terminal, is their interaction with its agent.
#[tokio::test]
async fn the_users_restore_of_a_failed_launch_engages_them() {
    let (temp, manager) = manager().await;
    manager.publish_config(global_config("[machine]\nmax_heavy_agents = 1\n"));
    let path = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None);
    let repo = repo.await.unwrap();
    let workspace = manager.create_workspace(&repo.id, "work".into(), None, None, None);
    let workspace = workspace.await.unwrap();
    let request = launch_in(&workspace, false);
    let entry = manager
        .agents
        .queue_add(&*manager, request)
        .await
        .unwrap()
        .entry
        .unwrap();
    (manager
        .agents
        .fail_queue_entry(&*manager, &entry.id, "gone"))
    .await
    .unwrap();

    let retry = QueueMethod::QueueRetry {
        entry: entry.id.clone(),
        start: RetryStart::Queued,
    };
    super::handle(&manager, retry, None)
        .await
        .map(drop)
        .unwrap();

    let interactions = manager.agents.interactions().await.unwrap();
    assert!(interactions.contains_key(&workspace.id));
}
