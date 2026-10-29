use std::time::Instant;

use tokio::io::{AsyncReadExt, BufReader};

use super::*;
use crate::protocol::RepositoryMethod::ListRepositories;
use crate::{
    daemon::{Server, questions::Ask, serve},
    protocol::{
        self, Body, DaemonMethod, ExecutionMethod, NotificationMethod, QuestionMethod, Request,
        Response,
    },
    test_support::{manager, repository},
};

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
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "asker".into(), None, None, None)
        .await
        .unwrap();
    let ask = Ask {
        question: "Which database?".into(),
        recommendation: "SQLite".into(),
        reason: None,
        choices: Vec::new(),
    };
    let question = manager
        .ask_question(&workspace.name, ask.clone())
        .await
        .unwrap();
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
    let await_answer = || QuestionMethod::AwaitAnswer {
        workspace: workspace.name.clone(),
        id: question.id,
        ask: ask.clone(),
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
        workspace: workspace.name.clone(),
        wrapper: wrapper.clone(),
        kind: crate::daemon::workspace::ExecutionKind::Command,
        agent: None,
        recover: false,
        reattach: true,
        queue_entry: None,
        demo: None,
        origin: None,
    };
    let watch = NotificationMethod::WatchNotifications {
        keep_unread: false,
        after: 0,
    };
    for refused in [Method::from(execute), watch.into()] {
        let Body::Error { message, .. } = reply(&mut send(socket, refused).await).await else {
            panic!("expected a refusal");
        };
        assert!(message.contains("limit of 1 long-lived"), "{message}");
    }
    let reattach = Method::Reattach(protocol::Reattachment::Reattach(protocol::Reattach {
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
    let answer = QuestionMethod::AnswerQuestion {
        id: question.id,
        answer: "SQLite".into(),
        shown: None,
    };
    let answered = reply(&mut send(socket, answer).await).await;
    assert!(matches!(answered, Body::Question(_)), "{answered:?}");
    let Body::Question(delivered) = reply(&mut waiting).await else {
        panic!("expected the wait to receive the answer");
    };
    assert_eq!(delivered.answer.as_deref(), Some("SQLite"));
    serving.abort();
}

#[test]
fn a_pending_restart_leaves_request_capacity_to_the_wrappers_it_restores() {
    let capacity = Capacity::new(1, 1);
    let restart = crate::protocol::ExecutionMethod::RestartAgent {
        execution: "agent".into(),
        join: false,
        run: None,
    };
    let _restart = capacity.admit(&restart.into()).unwrap();
    // The restored wrapper's own requests still find a slot.
    capacity.admit(&ListRepositories.into()).unwrap();
}

#[tokio::test]
async fn a_request_past_the_limit_is_refused_with_what_holds_the_capacity() {
    let capacity = Capacity::new(1, 1);
    let list = Method::from(ListRepositories);
    let held = capacity.admit(&list).unwrap();
    let Err(error) = capacity.admit(&list) else {
        panic!("a request past the limit was admitted");
    };
    let message = format!("{error:#}");
    assert!(message.contains("busy"), "{message}");
    assert!(message.contains("all 1 request slots"), "{message}");
    assert!(message.contains("1 list_repositories (oldest"), "{message}");
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
        let Body::Error { message, .. } = reply(&mut send(socket, ListRepositories).await).await
        else {
            panic!("expected the daemon to refuse the request as busy");
        };
        assert!(message.contains("busy"), "{message}");
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

#[test]
fn descriptor_exhaustion_and_aborted_clients_are_transient() {
    for code in [libc::EMFILE, libc::ENFILE, libc::ECONNABORTED, libc::EINTR] {
        assert!(transient(&io::Error::from_raw_os_error(code)), "{code}");
    }
    for code in [libc::EBADF, libc::EINVAL] {
        assert!(!transient(&io::Error::from_raw_os_error(code)), "{code}");
    }
}
