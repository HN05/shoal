use crate::scope::Caller;
use crate::stand_in::Fixture;
use shoal_protocol::protocol::AlertTarget;

#[tokio::test]
async fn only_the_agent_of_a_space_marks_its_turn() {
    let manager = Fixture::new().await;
    let repo = manager.repository("repo").await;
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    manager.run_agent(&parent.id).await;
    let user = manager
        .spaces
        .mark_chat_alert(&*manager, None, AlertTarget::Up, Vec::new());
    assert!(user.await.is_err());
    // A process of the space that runs no agent.
    let command = Caller::of_space(parent.id.clone());
    let marked =
        (manager.spaces).mark_chat_alert(&*manager, Some(&command), AlertTarget::Up, Vec::new());
    assert!(marked.await.is_err());
}
