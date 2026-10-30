use std::{path::PathBuf, sync::Arc, time::Duration};

use shoal_client::{
    Client, Connector, ErrorCode, GetWorkspace,
    wire::{self, FrameReader, HandshakeReply, Response},
};
use tokio::{
    net::{UnixListener, UnixStream},
    task::{JoinHandle, JoinSet},
};

use crate::{
    daemon::{Server, connections::Capacity, workspace::Manager},
    model::{Repository, Workspace},
    test_support::{git, manager, repository},
};

/// A generous guard against a hang; no test waits this long when it passes.
pub(super) const HANG: Duration = Duration::from_secs(60);

/// A daemon serving its socket from a temporary state directory, with a
/// registered repository whose origin no test reaches.
pub(super) struct TestDaemon {
    _root: tempfile::TempDir,
    pub manager: Arc<Manager>,
    pub repository: Repository,
    socket: PathBuf,
    accepting: JoinHandle<()>,
}

impl TestDaemon {
    pub async fn start() -> Self {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        git(&path, &["config", "protocol.allow", "never"]);
        let repository = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        git(
            &path,
            &["remote", "add", "origin", "https://github.com/team/repo"],
        );
        // The cached remote HEAD names the default branch without a fetch.
        git(
            &path,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let socket = root.path().join("api.sock");
        let accepting = accept(&socket, &manager);
        Self {
            _root: root,
            manager,
            repository,
            socket,
            accepting,
        }
    }

    /// Close every connection and listen again, as a restarted daemon does.
    pub async fn restart(&mut self) {
        self.accepting.abort();
        let _ = (&mut self.accepting).await;
        std::fs::remove_file(&self.socket).unwrap();
        self.accepting = accept(&self.socket, &self.manager);
    }

    pub fn connector(&self) -> Connector {
        Connector::new(&self.socket).name("test")
    }

    pub async fn client(&self) -> Client {
        self.connector().connect().await.unwrap()
    }

    /// A client calling as the agent of `workspace`.
    pub async fn scoped(&self, workspace: &Workspace) -> Client {
        self.connector()
            .scope(self.scope_token(workspace).await)
            .connect()
            .await
            .unwrap()
    }

    pub async fn scope_token(&self, workspace: &Workspace) -> String {
        let environment = self
            .manager
            .workspace_environment(&workspace.id)
            .await
            .unwrap();
        environment[crate::env::SCOPE_TOKEN].clone()
    }

    pub async fn workspace(&self, name: &str) -> Workspace {
        self.manager
            .create_workspace(
                &self.repository.id,
                name.into(),
                Some("HEAD".into()),
                None,
                None,
            )
            .await
            .unwrap()
    }

    /// A raw connection to the socket.
    pub async fn raw(
        &self,
    ) -> (
        FrameReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
    ) {
        let (reader, writer) = UnixStream::connect(&self.socket)
            .await
            .unwrap()
            .into_split();
        (FrameReader::new(reader), writer)
    }
}

/// Serve `socket` as the daemon does until the task is aborted, which ends
/// its connections with it.
fn accept(socket: &std::path::Path, manager: &Arc<Manager>) -> JoinHandle<()> {
    let listener = UnixListener::bind(socket).unwrap();
    let server = Server {
        capacity: Arc::new(Capacity::new(16, 16)),
        ..super::super::tests::server(manager.clone())
    };
    tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let server = server.clone();
            connections.spawn(async move {
                let operation = server.manager.gates.owned_operation().await;
                let _ = super::super::serve(stream, server, operation).await;
            });
        }
    })
}

/// The error code `result` failed with.
pub(super) fn code<T: std::fmt::Debug>(result: Result<T, shoal_client::Error>) -> ErrorCode {
    match result {
        Err(shoal_client::Error::Api(error)) => error.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn the_handshake_names_what_the_daemon_serves() {
    let daemon = TestDaemon::start().await;
    let client = daemon.client().await;
    let welcome = client.welcome();
    assert_eq!(welcome.version, shoal_client::VERSION);
    assert_eq!(welcome.supported_majors, [1]);
    assert!(welcome.offers("workspace.get"), "{welcome:?}");
    assert!(welcome.offers("subscribe"), "{welcome:?}");
    assert!(welcome.offers("topic:notifications"), "{welcome:?}");
}

#[tokio::test]
async fn an_unsupported_major_version_is_refused() {
    let daemon = TestDaemon::start().await;
    let (mut reader, mut writer) = daemon.raw().await;
    let hello = serde_json::json!({"client_api": {"version": "2.0.0", "client": "future"}});
    wire::write(&mut writer, &hello).await.unwrap();
    let reply: HandshakeReply = reader.read().await.unwrap().unwrap();
    let HandshakeReply::Error(error) = reply else {
        panic!("expected a refusal: {reply:?}");
    };
    assert_eq!(error.code, ErrorCode::UnsupportedVersion);
    assert!(error.message.contains("[1]"), "{}", error.message);
}

#[tokio::test]
async fn an_unknown_scope_is_refused_at_the_handshake() {
    let daemon = TestDaemon::start().await;
    let refused = daemon.connector().scope("unknown").connect().await;
    assert_eq!(code(refused.map(|_| ())), ErrorCode::ScopeDenied);
}

#[tokio::test]
async fn an_unknown_method_is_refused_and_the_connection_serves_the_next() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("api").await;
    let (mut reader, mut writer) = daemon.raw().await;
    let hello = serde_json::json!({"client_api": {"version": shoal_client::VERSION}});
    wire::write(&mut writer, &hello).await.unwrap();
    let _: HandshakeReply = reader.read().await.unwrap().unwrap();
    let unknown = serde_json::json!({"id": 1, "method": "workspace.teleport", "params": {}});
    wire::write(&mut writer, &unknown).await.unwrap();
    let response: Response = reader.read().await.unwrap().unwrap();
    assert_eq!(response.id, 1);
    let shoal_client::wire::Reply::Error(error) = response.reply else {
        panic!("expected a refusal: {response:?}");
    };
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    let get = serde_json::json!({"id": 2, "method": "workspace.get", "params": {"workspace": "api", "later": true}});
    wire::write(&mut writer, &get).await.unwrap();
    let response: Response = reader.read().await.unwrap().unwrap();
    let shoal_client::wire::Reply::Result(found) = response.reply else {
        panic!("expected the workspace: {response:?}");
    };
    assert_eq!(found["id"], workspace.id.as_str());
}

#[tokio::test]
async fn internal_requests_on_the_socket_are_answered_as_before() {
    use crate::protocol::{self, Body, DaemonMethod, Request};
    let daemon = TestDaemon::start().await;
    let (mut reader, mut writer) = daemon.raw().await;
    let request = Request {
        protocol: protocol::VERSION,
        id: 7,
        method: DaemonMethod::Status.into(),
        scope: None,
    };
    protocol::write(&mut writer, &request).await.unwrap();
    let response: protocol::Response = protocol::read(reader.get_mut()).await.unwrap();
    assert_eq!(response.id, 7);
    assert!(matches!(response.body, Body::Status(_)), "{response:?}");
}

#[tokio::test]
async fn a_call_after_a_restart_reconnects() {
    let mut daemon = TestDaemon::start().await;
    daemon.workspace("api").await;
    let client = daemon.client().await;
    let get = || GetWorkspace {
        workspace: "api".into(),
    };
    client.call(get()).await.unwrap();
    daemon.restart().await;
    // The closed connection fails the call it cuts off, if any; the next
    // call opens a new one.
    let _ = tokio::time::timeout(HANG, client.call(get()))
        .await
        .unwrap();
    let found = tokio::time::timeout(HANG, client.call(get()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.name, "api");
}
