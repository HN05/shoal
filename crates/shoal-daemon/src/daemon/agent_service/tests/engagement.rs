use shoal_protocol::space_names::SpaceSelector;

use shoal_agents::engagement;

use crate::daemon::{
    executions::ExecutionKind,
    test_support::{manager, repository},
};

#[tokio::test]
async fn a_prompt_the_user_submits_counts_as_interaction() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let (id, execution) = (workspace.id, execution.plan.id);
    (manager
        .spaces
        .reply_in_chat(&*manager, &SpaceSelector::id(&id), Some(&execution), None))
    .await
    .unwrap();
    let engagement = manager.store.read(move |db| engagement::get(db, &id));
    assert!(engagement.await.unwrap().interacted_at.is_some());
}
