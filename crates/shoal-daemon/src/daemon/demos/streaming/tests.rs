use std::collections::BTreeMap;

use shoal_protocol::{
    demos::{Launch, PORT, Preset},
    protocol::Response,
    space_names::SpaceSelector,
};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    net::{TcpListener, UnixStream},
};

use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        test_support::{manager, repository},
    },
    refusal::Reason,
};

/// A workspace running the demo `name` with `preset`, served on `port`.
async fn running(
    manager: &Manager,
    root: &std::path::Path,
    preset: Option<Preset>,
    port: u16,
) -> SpaceSelector {
    let checkout = repository(root, "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "tried".into(), None, None, None)
        .await
        .unwrap();
    let launch = Launch {
        id: "launch".into(),
        name: "site".into(),
        preset,
        open: None,
        scheme: None,
        viewer_command: None,
        environment: BTreeMap::from([(PORT.to_owned(), port.to_string())]),
        origin: crate::sessions::Origin::created(crate::sessions::HostName::Zmx, "site".into()),
    };
    manager
        .begin_execution_running(
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(launch),
        )
        .await
        .unwrap();
    SpaceSelector::id(&workspace.id)
}

#[tokio::test]
async fn a_web_demo_s_stream_carries_one_connection_both_ways() {
    let (temp, manager) = manager().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let workspace = running(&manager, temp.path(), Some(Preset::Web), port).await;
    let open = manager.open_demo(&workspace, "site", None).await.unwrap();
    let (client, daemon) = UnixStream::pair().unwrap();
    let guard = manager.gates.owned_operation().await;
    let relay = tokio::spawn(manager.clone().relay_demo(daemon, 7, (open, None), guard));
    let (mut served, _) = listener.accept().await.unwrap();
    let mut client = BufReader::new(client);
    let mut answer = String::new();
    client.read_line(&mut answer).await.unwrap();
    let answer: Response = serde_json::from_str(&answer).unwrap();
    assert_eq!(answer.id, 7);
    let demo = Box::<shoal_protocol::demos::Running>::try_from(answer.body).unwrap();
    assert_eq!(demo.launch.name, "site");
    // The client's end of its bytes reaches the demo, whose reply then
    // comes back.
    client.write_all(b"ping").await.unwrap();
    client.get_mut().shutdown().await.unwrap();
    let mut received = Vec::new();
    served.read_to_end(&mut received).await.unwrap();
    assert_eq!(received, b"ping");
    served.write_all(b"pong").await.unwrap();
    drop(served);
    let mut replied = Vec::new();
    client.read_to_end(&mut replied).await.unwrap();
    assert_eq!(replied, b"pong");
    relay.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_terminal_demo_is_refused_a_stream() {
    let (temp, manager) = manager().await;
    let workspace = running(&manager, temp.path(), None, 1).await;
    let refused = manager
        .open_demo(&workspace, "site", None)
        .await
        .err()
        .unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady));
    assert!(refused.to_string().contains("terminal"), "{refused:#}");
}

#[tokio::test]
async fn a_demo_that_takes_no_connection_is_refused_a_stream() {
    let (temp, manager) = manager().await;
    // A port nothing listens on any longer.
    let port = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let workspace = running(&manager, temp.path(), Some(Preset::Server), port).await;
    let refused = manager
        .open_demo(&workspace, "site", None)
        .await
        .err()
        .unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady));
}
