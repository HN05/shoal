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
        space: SpaceSelector::id(&workspace.id),
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

/// A registered repository with a chat in its checkout.
async fn chat_fixture() -> (tempfile::TempDir, Arc<Manager>, crate::model::Chat) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.into(),
            "talk".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    (root, manager, chat)
}

/// What a wrapper in this process asks to run `agent`, or a plain command,
/// in the space `space`.
fn context(space: &crate::client::space_ids::SpaceId, agent: Option<&str>) -> ExecutionContext {
    ExecutionContext {
        space: SpaceSelector::id(space),
        wrapper: crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap(),
        kind: ExecutionKind::Command,
        agent: agent.map(crate::execution::TrackedAgent::named),
        recover: true,
        reattach: true,
        queue_entry: None,
        demo: None,
        origin: None,
        merge: None,
        caller: None,
    }
}

/// The error a refused registration answers with.
fn refusal(registered: Result<Registered, Box<Body>>) -> String {
    match registered.map(drop).unwrap_err().as_ref() {
        Body::Error { message, .. } => message.clone(),
        body => panic!("{body:?}"),
    }
}

#[tokio::test]
async fn a_chats_agent_runs_as_an_execution_of_its_chat_with_the_chats_scope() {
    let (_root, manager, chat) = chat_fixture().await;

    let registered = register(&manager, context(&chat.id, Some("claude")))
        .await
        .map_err(|refusal| format!("{refusal:?}"))
        .unwrap();

    let plan = &registered.plan;
    assert_eq!(plan.space.kind, crate::model::SpaceKind::Chat);
    assert_eq!(plan.space.id, chat.id);
    let checkout = manager.chat_checkout_path(&chat).await.unwrap();
    assert_eq!(plan.space.path, checkout);
    assert!(plan.ports.is_empty());
    let tmp_dir = chat.tmp_dir.clone().unwrap();
    assert_eq!(
        plan.environment,
        [(crate::env::TMPDIR.to_owned(), tmp_dir.into_os_string())].into()
    );
    let token = manager.chat_scope(&chat.id).await.unwrap();
    assert_eq!(plan.scope_token, token);
    let caller = manager.caller(&token).await.unwrap().unwrap();
    assert_eq!(caller.space_id, chat.id);
    assert_eq!(caller.execution_id(), Some(&plan.id));
    assert!(manager.caller_agent(&caller).await.is_some());
}

#[tokio::test]
async fn a_chat_runs_its_one_agent_and_no_commands() {
    let (_root, manager, chat) = chat_fixture().await;

    let command = refusal(register(&manager, context(&chat.id, None)).await);
    assert!(command.contains("runs only its agent"), "{command}");
    let _agent = register(&manager, context(&chat.id, Some("claude")))
        .await
        .map_err(|refusal| format!("{refusal:?}"))
        .unwrap();
    let second = refusal(register(&manager, context(&chat.id, Some("codex"))).await);
    assert!(second.contains("already runs its agent"), "{second}");
}
