use crate::daemon::test_support::manager;
use crate::time::UnixTime;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::executions::ServiceLimit;
use shoal_protocol::executions::UpstreamLimit;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_chats_agent_reports_its_limit_too() {
    let (_temp, manager) = manager().await;
    let chat = crate::daemon::test_support::chat(&manager, "master", "chat", None, None).await;
    let limit = UpstreamLimit {
        limit: ServiceLimit::Exhausted,
        until: Some(UnixTime::from_seconds(5_000)),
    };
    let execution = ExecutionId::from("chat-agent");
    manager
        .agents
        .report_upstream_limit(
            &*manager,
            &SpaceSelector::id(&chat.id),
            execution.clone(),
            Some(limit),
        )
        .await
        .unwrap();
    assert_eq!(
        manager
            .agents
            .upstream_limits()
            .lock()
            .unwrap()
            .get(&execution),
        Some(&limit)
    );
}
