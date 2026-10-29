use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, BufReader},
    sync::oneshot,
};

use super::*;
use crate::{
    protocol::{AgentMethod, Request, WorkspaceMethod},
    test_support::manager,
};

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

async fn reply(stream: UnixStream) -> Body {
    let response: Response = bounded(protocol::read_buffered(&mut BufReader::new(stream)))
        .await
        .unwrap();
    response.body
}

#[tokio::test]
async fn startup_answers_status_with_its_step_and_serves_other_requests_after() {
    let (_root, manager) = manager().await;
    let socket = manager.paths.socket.clone();
    let startup = Startup::begin(UnixListener::bind(&socket).unwrap());
    let (unblock, blocked) = oneshot::channel::<()>();
    let audit = startup.step("auditing worktrees", blocked);
    let clients = async {
        let waiting = send(&socket, 7, WorkspaceMethod::ListWorkspaces).await;
        let wake = AgentMethod::AwaitAgentWake {
            workspace: "w".into(),
            due: false,
        };
        let mut retried = send(&socket, 8, wake).await;
        let Body::Error { message, .. } = reply(send(&socket, 9, DaemonMethod::Status).await).await
        else {
            panic!("expected the startup step");
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
        waiting
    };
    let (waiting, audited) = tokio::join!(clients, audit);
    audited.unwrap();
    let (_listener, _connections) = startup.finish(crate::daemon::tests::server(manager)).await;
    let served = reply(waiting).await;
    assert!(matches!(served, Body::Workspaces(_)), "{served:?}");
}

#[tokio::test(start_paused = true)]
async fn a_client_that_never_sends_its_request_does_not_hold_up_serving() {
    let (_root, manager) = manager().await;
    let socket = manager.paths.socket.clone();
    let startup = Startup::begin(UnixListener::bind(&socket).unwrap());
    let _silent = UnixStream::connect(&socket).await.unwrap();
    // Accepted after the silent client, so it proves that one was accepted.
    reply(send(&socket, 1, DaemonMethod::Status).await).await;
    let finishing = Instant::now();
    let (_listener, _connections) = startup.finish(crate::daemon::tests::server(manager)).await;
    assert!(finishing.elapsed() < timing::REQUEST_READ_TIMEOUT);
}
