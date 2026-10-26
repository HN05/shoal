use super::*;
use crate::test_support::{manager, repository};

#[tokio::test]
async fn a_reported_retry_shows_until_the_proxy_or_its_execution_ends() {
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
            &workspace.id,
            None,
            super::super::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let execution = started.plan.id.clone();
    let executions = || async {
        manager
            .inspect_workspace(&workspace.name)
            .await
            .unwrap()
            .executions
    };
    let retry = ProxyRetry {
        failure: TurnFailure::Overloaded,
        attempt: 2,
        next_at: 1_000,
    };
    manager
        .report_proxy_retry(&workspace.name, execution.clone(), Some(retry.clone()))
        .await
        .unwrap();
    assert_eq!(
        manager.proxy_retry(&executions().await),
        Some(retry.clone())
    );
    manager
        .report_proxy_retry(&workspace.name, execution.clone(), None)
        .await
        .unwrap();
    assert_eq!(manager.proxy_retry(&executions().await), None);

    manager
        .report_proxy_retry(&workspace.name, execution.clone(), Some(retry))
        .await
        .unwrap();
    manager
        .finish_execution(execution, super::super::ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    assert!(manager.proxy_retries.lock().unwrap().is_empty());
}
