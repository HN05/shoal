use std::sync::atomic::Ordering;

use shoal_client::{
    CancelOperation, CreateWorkspace, ErrorCode, Followed, LandWorkspace, OperationResult,
    OutputStream, Progress, SplitWorkspace, Step,
};

use super::super::super::tests::{TestDaemon, code};
use crate::{
    daemon::operations::fake,
    protocol::operations::{
        Ended, Failure, OperationSpec, Outcome, Prepared, Report, Step as InternalStep,
    },
};

/// Every event and the result of following `operation` from `after` on.
async fn followed(
    daemon: &TestDaemon,
    operation: &str,
    after: u64,
) -> (Vec<Progress>, Result<OperationResult, shoal_client::Error>) {
    let mut following = daemon
        .connector()
        .follow(operation.into(), after)
        .await
        .unwrap();
    let mut progress = Vec::new();
    loop {
        match following.next().await {
            Ok(Followed::Progress(event)) => progress.push(event.progress),
            Ok(Followed::Ended(result)) => return (progress, Ok(result)),
            Err(error) => return (progress, Err(error)),
        }
    }
}

#[tokio::test]
async fn a_created_workspace_is_set_up_in_an_operation_that_clients_follow() {
    let daemon = TestDaemon::start().await;
    let client = daemon.client().await;
    let started = client
        .call(CreateWorkspace {
            repository: daemon.repository.id.clone(),
            name: "api".into(),
            base: Some("HEAD".into()),
            issue: None,
            pr: None,
            agent: None,
        })
        .await
        .unwrap();
    assert_eq!(started.workspace.name, "api");
    let mut worker = fake::take(&started.operation);
    let OperationSpec::Prepare { workspace, steps } = worker.order.spec.clone() else {
        panic!("expected a preparation: {:?}", worker.order);
    };
    assert_eq!(workspace, started.workspace.id);
    assert!(steps.post_setup && steps.pr.is_none() && steps.agent.is_none());
    let manager = &daemon.manager;
    let report = |report| manager.report_operation(&started.operation, report, None);
    report(Report::Step(InternalStep::SetupStarted))
        .await
        .unwrap();
    worker.stderr("installing").await;
    let prepared = Prepared {
        workspace: Box::new(manager.workspace(&workspace).await.unwrap()),
        agent: None,
    };
    let outcome = Box::new(Outcome::Prepared(prepared));
    report(Report::Ended(Ended::Succeeded(outcome)))
        .await
        .unwrap();
    worker.exit(0);
    let (progress, result) = followed(&daemon, &started.operation, 0).await;
    let installing = Progress::Output {
        stream: OutputStream::Stderr,
        line: "installing".into(),
    };
    let setup = Progress::Step {
        step: Step::SetupStarted,
    };
    assert_eq!(progress, [setup, installing]);
    let Ok(OperationResult::Prepared { workspace, agent }) = result else {
        panic!("expected the prepared workspace: {result:?}");
    };
    assert_eq!((workspace.name.as_str(), agent), ("api", None));
    // Following again after what was read gives only the end.
    let (progress, result) = followed(&daemon, &started.operation, 2).await;
    assert!(progress.is_empty() && result.is_ok(), "{progress:?}");
}

#[tokio::test]
async fn a_failed_setup_ends_following_with_its_code_and_a_cancel_interrupts_the_worker() {
    let daemon = TestDaemon::start().await;
    let parent = daemon.workspace("parent").await;
    let client = daemon.scoped(&parent).await;
    let started = client
        .call(SplitWorkspace {
            workspace: "parent".into(),
            name: "child".into(),
            base: Some("HEAD".into()),
            issue: None,
            agent: None,
        })
        .await
        .unwrap();
    let worker = fake::take(&started.operation);
    assert!(
        worker.token.is_some(),
        "the worker acts with the caller's scope"
    );
    let cancel = CancelOperation {
        operation: started.operation.clone(),
    };
    client.call(cancel).await.unwrap();
    assert!(worker.interrupted.load(Ordering::SeqCst));
    let failure = Failure::SetupFailed {
        message: "setup command exited with status 2".into(),
    };
    let report = Report::Ended(Ended::Failed(failure));
    daemon
        .manager
        .report_operation(&started.operation, report, None)
        .await
        .unwrap();
    worker.exit(0);
    let (_, result) = followed(&daemon, &started.operation, 0).await;
    assert_eq!(code(result), ErrorCode::SetupFailed);
}

#[tokio::test]
async fn operations_outside_the_callers_tree_are_refused() {
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    daemon.workspace("other").await;
    let scoped = daemon.scoped(&own).await;
    let land = |workspace: &str| LandWorkspace {
        workspace: workspace.into(),
        push: None,
    };
    assert_eq!(
        code(scoped.call(land("other")).await),
        ErrorCode::ScopeDenied
    );
    let theirs = daemon.client().await.call(land("other")).await.unwrap();
    fake::take(&theirs.operation).exit(1);
    let token = daemon.scope_token(&own).await;
    let follow = |operation: &str| {
        daemon
            .connector()
            .scope(token.clone())
            .follow(operation.into(), 0)
    };
    let mut refused = follow(&theirs.operation).await.unwrap();
    assert_eq!(code(refused.next().await), ErrorCode::ScopeDenied);
    let mut missing = follow("missing").await.unwrap();
    assert_eq!(code(missing.next().await), ErrorCode::NotFound);
}

#[tokio::test]
async fn creation_refuses_a_bad_agent_or_issue_without_leaving_a_workspace() {
    let daemon = TestDaemon::start().await;
    let client = daemon.client().await;
    let create = |issue: Option<&str>, agent: Option<&str>| CreateWorkspace {
        repository: daemon.repository.id.clone(),
        name: "api".into(),
        base: Some("HEAD".into()),
        issue: issue.map(Into::into),
        pr: None,
        agent: agent.map(|agent| shoal_client::AgentToQueue {
            agent: agent.into(),
            prompt: None,
        }),
    };
    assert_eq!(
        code(client.call(create(None, Some("Not An Agent"))).await),
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        code(client.call(create(None, Some("undefined"))).await),
        ErrorCode::OperationFailed
    );
    assert!(
        client
            .call(create(Some("not an issue"), None))
            .await
            .is_err()
    );
    let get = shoal_client::GetWorkspace {
        workspace: "api".into(),
    };
    assert_eq!(code(client.call(get).await), ErrorCode::NotFound);
}
