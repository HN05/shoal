use std::time::Instant;

use tokio::io::{AsyncReadExt, BufReader};

use super::*;
use crate::protocol::RepositoryMethod::ListRepositories;
use crate::{
    daemon::{
        Server, serve,
        test_support::{manager, repository},
    },
    protocol::{
        self, AlertMethod, Body, DaemonMethod, ExecutionMethod, Request, Response, ThreadMethod,
    },
};
use shoal_protocol::space_names::SpaceSelector;

/// A hang guard, not the behavior under test.
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("connection exchange timed out")
}

async fn send(socket: &std::path::Path, method: impl Into<Method>) -> BufReader<UnixStream> {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let request = Request {
        protocol: protocol::VERSION,
        id: 1,
        method: method.into(),
        scope: None,
    };
    protocol::write(&mut stream, &request).await.unwrap();
    BufReader::new(stream)
}

async fn reply(stream: &mut BufReader<UnixStream>) -> Body {
    let response: Response = bounded(protocol::read_buffered(stream)).await.unwrap();
    response.body
}

#[tokio::test]
async fn requests_are_served_while_long_lived_capacity_is_full() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "asker".into(), None, None, None)
        .await
        .unwrap();
    let question =
        crate::daemon::test_support::ask(&manager, &workspace.id, "Which database?").await;
    let socket = &manager.paths.socket.clone();
    let listener = UnixListener::bind(socket).unwrap();
    // One long-lived slot, which the first wait takes.
    let capacity = Arc::new(Capacity::new(1, 1));
    let server = Server {
        manager: manager.clone(),
        started: Instant::now(),
        managed: false,
        exit: tokio::sync::mpsc::channel(1).0,
        capacity: capacity.clone(),
    };
    let serving = tokio::spawn(async move {
        loop {
            let stream = accept(&listener).await.unwrap();
            let server = server.clone();
            let operation = manager.gates.owned_operation().await;
            tokio::spawn(serve(stream, server, operation));
        }
    });
    let await_answer = || ThreadMethod::AwaitPost {
        space: SpaceSelector::id(&workspace.id),
        item: question.number.item,
        after: question.posts[0].id,
    };
    let mut waiting = send(socket, await_answer()).await;
    bounded(async {
        while capacity.long_lived.permits.available_permits() > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let execute = ExecutionMethod::Execute {
        new_session: false,
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        space: SpaceSelector::id(&workspace.id),
        wrapper: wrapper.clone(),
        kind: crate::daemon::executions::ExecutionKind::Command,
        agent: None,
        recover: false,
        source: None,
        reattach: true,
        queue_entry: None,
        demo: None,
        origin: None,
        merge: None,
    };
    let watch = AlertMethod::WatchAlerts {
        keep_unread: false,
        after: 0,
    };
    for refused in [Method::from(execute), watch.into()] {
        let reply = reply(&mut send(socket, refused).await).await;
        assert!(matches!(reply, Body::Error { .. }), "{reply:?}");
    }
    let reattach = Method::Reattach(protocol::Reattachment::Reattach(protocol::Reattach {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        execution: "running".into(),
        wrapper,
        child: None,
        group_id: std::process::id(),
        scope_token: "token".into(),
        agent: None,
        recover: false,
        running_ms: 0,
        awaiting_restore: None,
        between_runs: false,
        reports_status: false,
    }));
    // Clients that retry a connection closed unanswered keep waiting.
    for retried in [Method::from(await_answer()), reattach] {
        let mut unanswered = Vec::new();
        bounded(send(socket, retried).await.read_to_end(&mut unanswered))
            .await
            .unwrap();
        assert!(
            unanswered.is_empty(),
            "{}",
            String::from_utf8_lossy(&unanswered)
        );
    }
    let status = reply(&mut send(socket, DaemonMethod::Status).await).await;
    assert!(matches!(status, Body::Status(_)), "{status:?}");
    let answer = ThreadMethod::Post {
        thread: shoal_protocol::threads::ThreadNumber::Full(question.number),
        post: shoal_protocol::threads::NewPost {
            text: "SQLite".into(),
            ..Default::default()
        },
        attachments: Vec::new(),
        code: Vec::new(),
        changes: Vec::new(),
        from: None,
    };
    let answered = reply(&mut send(socket, answer).await).await;
    assert!(matches!(answered, Body::SpaceThread(_)), "{answered:?}");
    let Body::SpaceThread(delivered) = reply(&mut waiting).await else {
        panic!("expected the wait to receive the answer");
    };
    assert_eq!(delivered.posts.last().unwrap().post.text, "SQLite");
    serving.abort();
}

#[test]
fn a_pending_restart_leaves_request_capacity_to_the_wrappers_it_restores() {
    let capacity = Capacity::new(1, 1);
    let restart = crate::protocol::AgentMethod::RestartAgent {
        space: shoal_protocol::space_names::SpaceSelector::id(&"agent".into()),
        restart: Default::default(),
    };
    let _restart = capacity.admit(&restart.into()).unwrap();
    // The restored wrapper's own requests still find a slot.
    capacity.admit(&ListRepositories.into()).unwrap();
}

#[tokio::test]
async fn a_request_past_the_limit_is_refused_until_one_finishes() {
    let capacity = Capacity::new(1, 1);
    let list = Method::from(ListRepositories);
    let held = capacity.admit(&list).unwrap();
    assert!(capacity.admit(&list).is_err());
    // Daemon administration needs no slot.
    capacity.admit(&DaemonMethod::Status.into()).unwrap();
    drop(held);
    capacity.admit(&list).unwrap();
}

#[tokio::test]
async fn connections_are_accepted_and_answered_while_requests_hold_every_slot() {
    let (_root, manager) = manager().await;
    let socket = &manager.paths.socket.clone();
    let listener = UnixListener::bind(socket).unwrap();
    let server = Server {
        manager: manager.clone(),
        started: Instant::now(),
        managed: false,
        exit: tokio::sync::mpsc::channel(1).0,
        capacity: Arc::new(Capacity::new(1, 1)),
    };
    let _held = server.capacity.admit(&ListRepositories.into()).unwrap();
    let serving = tokio::spawn(async move {
        loop {
            let stream = accept(&listener).await.unwrap();
            let operation = manager.gates.owned_operation().await;
            tokio::spawn(serve(stream, server.clone(), operation));
        }
    });
    // More clients than the slots, each answered rather than left queued.
    for _ in 0..3 {
        let reply = reply(&mut send(socket, ListRepositories).await).await;
        assert!(matches!(reply, Body::Error { .. }), "{reply:?}");
    }
    // Status still answers, so recovery commands can act on the daemon.
    let status = reply(&mut send(socket, DaemonMethod::Status).await).await;
    assert!(matches!(status, Body::Status(_)), "{status:?}");
    serving.abort();
}

#[test]
fn the_descriptor_limit_reaches_the_target_or_stays_higher() {
    let hard = descriptor_limit().unwrap().rlim_max;
    let limit = raise_descriptor_limit();
    assert!(limit >= hard.min(DESCRIPTOR_TARGET), "{limit}");
    assert_eq!(descriptor_limit().unwrap().rlim_cur, limit);
}
