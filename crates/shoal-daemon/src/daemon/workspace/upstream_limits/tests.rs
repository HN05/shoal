use super::*;
use crate::client::executions::ServiceLimit;
use crate::daemon::test_support::{manager, repository};

#[test]
fn a_limit_stands_until_its_named_end_or_for_a_while_without_one() {
    let now = UnixTime::from_seconds(1_000);
    let named = UpstreamLimit {
        limit: ServiceLimit::RateLimited,
        until: Some(UnixTime::from_seconds(5_000)),
    };
    assert_eq!(standing(named, now), named);
    let unnamed = UpstreamLimit {
        limit: ServiceLimit::Exhausted,
        until: None,
    };
    assert_eq!(standing(unnamed, now).until, Some(now + UNNAMED_LIMIT_FOR));
}

#[tokio::test]
async fn a_reported_limit_stands_until_the_proxy_or_its_execution_ends() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let started = manager
        .begin_execution(
            workspace.id.as_str(),
            None,
            super::super::ExecutionKind::Command,
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
    let limits = || manager.upstream_limits.lock().unwrap().clone();
    manager
        .report_upstream_limit(&selector, execution.clone(), Some(limit))
        .await
        .unwrap();
    assert_eq!(limits().get(&execution), Some(&limit));
    manager
        .report_upstream_limit(&selector, execution.clone(), None)
        .await
        .unwrap();
    assert!(limits().is_empty());

    manager
        .report_upstream_limit(&selector, execution.clone(), Some(limit))
        .await
        .unwrap();
    manager
        .finish_execution(execution, super::super::ExecutionKind::Command, Some(0))
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
        .report_upstream_limit(&SpaceSelector::id(&chat.id), execution.clone(), Some(limit))
        .await
        .unwrap();
    assert_eq!(
        manager.upstream_limits.lock().unwrap().get(&execution),
        Some(&limit)
    );
}
