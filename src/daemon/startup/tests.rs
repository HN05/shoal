use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, BufReader},
    sync::oneshot,
};

use super::*;
use crate::protocol::{AgentMethod, WorkspaceMethod};

/// A hang guard, not the behavior under test.
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("startup exchange timed out")
}

async fn send(socket: &std::path::Path, id: u64, method: impl Into<Method>) -> UnixStream {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let request = Request {
        protocol: protocol::VERSION,
        id,
        method: method.into(),
        scope: None,
    };
    protocol::write(&mut stream, &request).await.unwrap();
    stream
}

#[tokio::test]
async fn startup_answers_status_with_its_step_and_holds_other_requests() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let socket = root.path().join("daemon.sock");
    let startup = Startup::begin(UnixListener::bind(&socket).unwrap());
    let (unblock, blocked) = oneshot::channel::<()>();
    let audit = startup.step("auditing worktrees", blocked);
    let clients = async {
        // Accepted in order, so the status reply proves the earlier ones were read.
        let _held = send(&socket, 7, WorkspaceMethod::ListWorkspaces).await;
        let wake = AgentMethod::AwaitAgentWake {
            workspace: "w".into(),
            due: false,
        };
        let mut retried = send(&socket, 8, wake).await;
        let mut status = BufReader::new(send(&socket, 9, DaemonMethod::Status).await);
        let response: Response = bounded(protocol::read_buffered(&mut status)).await.unwrap();
        let Body::Error { message, .. } = response.body else {
            panic!("expected the startup step, got {:?}", response.body);
        };
        assert!(
            message.contains("starting: auditing worktrees for"),
            "{message}"
        );
        // A request its client sends again is closed unanswered.
        let mut unanswered = Vec::new();
        bounded(retried.read_to_end(&mut unanswered)).await.unwrap();
        assert!(unanswered.is_empty());
        unblock.send(()).unwrap();
        _held
    };
    let (_held, audited) = tokio::join!(clients, audit);
    audited.unwrap();
    let (_listener, held) = bounded(startup.finish()).await;
    let ids = held
        .iter()
        .map(|(_, request)| request.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, [7]);
}
