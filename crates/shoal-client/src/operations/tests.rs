use crate::connection::{self, Connector};
use crate::operations::{Followed, Following};
use crate::test_daemon::{Connect as _, start_workspace};
use shoal_daemon::test_daemon::{HANG, TestDaemon, fake};
use shoal_protocol::protocol::{
    ErrorCode, OperationMethod, RemoteError,
    operations::{Ended, Failure, OperationSpec, Report, StartedWorkspace, WorkspaceStart},
};
use shoal_protocol::space_names::SpaceSelector;

/// Follow operation `id` from its start to its end.
async fn follow(app: &Connector, id: &str) -> Ended {
    let mut following = Following::open(app, id.into(), 0).await.unwrap();
    tokio::time::timeout(HANG, async {
        loop {
            if let Followed::Ended(ended) = following.next().await.unwrap() {
                return ended;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_created_workspace_is_prepared_in_an_operation_the_app_follows() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let create = start_workspace(&daemon, WorkspaceStart::Name("api".into()));
    let started: StartedWorkspace = connection::request(&app, create).await.unwrap();
    assert_eq!(started.workspace.name, "api");
    assert!(!started.reused);
    let worker = fake::take(&started.operation);
    let OperationSpec::Prepare { workspace, steps } = worker.order.spec.clone() else {
        panic!("expected a preparation: {:?}", worker.order);
    };
    assert_eq!(workspace, SpaceSelector::id(&started.workspace.id));
    assert!(steps.post_setup && steps.pr.is_none() && steps.agent.is_none());
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
    let Ended::Failed(failure) = follow(&app, &started.operation).await else {
        panic!("expected the failure");
    };
    assert_eq!(failure.code(), ErrorCode::SetupFailed);
}

#[tokio::test]
async fn following_ends_once_the_followers_scope_token_is_revoked() {
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    let merge = OperationMethod::StartOperation {
        spec: OperationSpec::Merge {
            workspace: SpaceSelector::id(&own.id),
            options: Default::default(),
        },
        env: None,
    };
    let started: shoal_protocol::protocol::operations::Started =
        connection::request(&daemon.connector(), merge)
            .await
            .unwrap();
    let mut worker = fake::take(&started.id);
    let token = daemon.scope_token(&own).await;
    let agent = daemon.connector().scope(token.clone());
    let mut following = Following::open(&agent, started.id.clone(), 0)
        .await
        .unwrap();
    worker.stdout("one").await;
    let first = following.next().await.unwrap();
    assert!(matches!(first, Followed::Event(_)), "{first:?}");
    daemon
        .manager
        .revoke_scope(&SpaceSelector::id(&own.id), token)
        .await
        .unwrap();
    worker.stdout("two").await;
    let error = following.next().await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<RemoteError>().unwrap().code,
        ErrorCode::ScopeDenied
    );
    worker.exit(0);
}
