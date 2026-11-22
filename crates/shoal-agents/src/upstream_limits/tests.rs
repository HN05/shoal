use super::*;
use crate::stand_in::Fixture;
use crate::time::UnixTime;
use shoal_protocol::executions::ServiceLimit;
use shoal_protocol::executions::UpstreamLimit;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

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
    let fixture = Arc::new(Fixture::new().await);
    let workspace = fixture.workspace("agent").await;
    let started = fixture
        .agents
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            crate::executions::ExecutionKind::Command,
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
    let limits = || fixture.agents.upstream_limits().lock().unwrap().clone();
    fixture
        .agents
        .report_upstream_limit(&*fixture.daemon, &selector, execution.clone(), Some(limit))
        .await
        .unwrap();
    assert_eq!(limits().get(&execution), Some(&limit));
    fixture
        .agents
        .report_upstream_limit(&*fixture.daemon, &selector, execution.clone(), None)
        .await
        .unwrap();
    assert!(limits().is_empty());

    fixture
        .agents
        .report_upstream_limit(&*fixture.daemon, &selector, execution.clone(), Some(limit))
        .await
        .unwrap();
    fixture
        .agents
        .finish_execution(
            &*fixture.daemon,
            execution,
            crate::executions::ExecutionKind::Command,
            Some(0),
        )
        .await
        .unwrap();
    assert!(limits().is_empty());
}
