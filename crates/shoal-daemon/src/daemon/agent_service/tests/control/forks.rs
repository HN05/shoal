use crate::{
    daemon::test_support::{chat, manager, report_session, repository},
    model::AgentSession,
};
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_fork_starts_from_a_chats_agent() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = chat(&manager, "plan", "plan", Some(&repo), None).await;
    report_session(&manager, &chat.id, "s1").await;
    assert_eq!(
        manager
            .agents
            .agent_session(&*manager, &SpaceSelector::id(&chat.id), None)
            .await
            .unwrap(),
        AgentSession {
            agent: "claude".into(),
            session: "s1".into(),
        }
    );
}
