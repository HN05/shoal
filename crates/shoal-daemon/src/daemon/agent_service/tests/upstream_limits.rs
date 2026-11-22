use crate::daemon::test_support::{manager, repository};
use crate::time::UnixTime;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::executions::ServiceLimit;
use shoal_protocol::executions::UpstreamLimit;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_reported_limit_stands_until_the_proxy_or_its_execution_ends() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let started = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            crate::daemon::executions::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let execution = started.plan.id.clone();
    let selector = SpaceSelector::id(&workspace.id);
    let limit = UpstreamLimit {
        limit: ServiceLimit::RateLimited,
        until: Some(UnixTime::from_seconds(5_000)),
    };
    let limits = || manager.agents.upstream_limits().lock().unwrap().clone();
    manager
        .agents
        .report_upstream_limit(&*manager, &selector, execution.clone(), Some(limit))
        .await
        .unwrap();
    assert_eq!(limits().get(&execution), Some(&limit));
    manager
        .agents
        .report_upstream_limit(&*manager, &selector, execution.clone(), None)
        .await
        .unwrap();
    assert!(limits().is_empty());

    manager
        .agents
        .report_upstream_limit(&*manager, &selector, execution.clone(), Some(limit))
        .await
        .unwrap();
    manager
        .agents
        .finish_execution(
            &*manager,
            execution,
            crate::daemon::executions::ExecutionKind::Command,
            Some(0),
        )
        .await
        .unwrap();
    assert!(limits().is_empty());
}

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
