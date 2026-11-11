use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::execution_connection::execute;
use crate::daemon::test_support::{manager, repository};
use crate::state::WorkspaceState;
use tokio::{io::BufReader, sync::oneshot};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(60), future)
        .await
        .expect("registration stalled")
}

#[tokio::test(start_paused = true)]
async fn a_slow_registration_keeps_its_wrapper_waiting_until_it_leaves() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let (finish, finished) = oneshot::channel::<()>();
    let registering = tokio::spawn(async move { keeping_alive(&mut server, 7, finished).await.1 });
    let mut client = BufReader::new(client);
    for _ in 0..3 {
        let response: Response = bounded(protocol::read_buffered(&mut client)).await.unwrap();
        assert_eq!(response.id, 7);
        assert!(
            matches!(response.body, Body::Keepalive),
            "{:?}",
            response.body
        );
    }
    drop(client);
    tokio::time::sleep(timing::REGISTRATION_KEEPALIVE * 2).await;
    finish.send(()).unwrap();
    let listening = bounded(registering).await.unwrap();
    assert!(!listening);
}

#[tokio::test]
async fn a_setup_its_wrapper_never_received_fails_as_not_started() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "unreached".into(), None, None, None)
        .await
        .unwrap();
    std::fs::write(
        workspace.path.join(".shoal.toml"),
        "setup.command = 'setup.sh'\n",
    )
    .unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    // The wrapper gave up before the daemon registered its setup.
    drop(client);
    let context = ExecutionContext {
        workspace: SpaceSelector::id(&workspace.id),
        wrapper: crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap(),
        kind: ExecutionKind::Setup,
        agent: None,
        recover: false,
        reattach: false,
        queue_entry: None,
        demo: None,
        origin: None,
        merge: None,
        caller: None,
    };
    let operation = manager.gates.owned_operation().await;
    bounded(execute(server, 1, manager.clone(), context, operation))
        .await
        .unwrap();

    let selector = SpaceSelector::id(&workspace.id);
    let inspection = manager.inspect_workspace(&selector).await.unwrap();
    assert!(
        inspection.executions.is_empty(),
        "{:?}",
        inspection.executions
    );
    assert_eq!(inspection.workspace.state, WorkspaceState::Failed);
    let error = inspection.workspace.error.unwrap_or_default();
    assert!(error.contains("did not start"), "{error}");
    assert!(!manager.executions.has_connections().await);
}
