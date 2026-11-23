use crate::time::UnixTime;
use crate::{
    daemon::test_support::{manager, repository},
    state::TurnFailure,
};
use shoal_protocol::executions::ProxyRetry;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_reported_retry_shows_until_the_proxy_or_its_execution_ends() {
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
    let executions = || async {
        manager
            .spaces
            .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .executions
    };
    let retry = ProxyRetry {
        failure: TurnFailure::Overloaded,
        attempt: 2,
        next_at: UnixTime::from_seconds(1_000),
    };
    manager
        .agents
        .report_proxy_retry(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            execution.clone(),
            Some(retry.clone()),
        )
        .await
        .unwrap();
    assert_eq!(
        manager.agents.proxy_retry(&executions().await),
        Some(retry.clone())
    );
    manager
        .agents
        .report_proxy_retry(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            execution.clone(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(manager.agents.proxy_retry(&executions().await), None);

    manager
        .agents
        .report_proxy_retry(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            execution.clone(),
            Some(retry),
        )
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
    assert!(manager.agents.proxy_retries().lock().unwrap().is_empty());
}
