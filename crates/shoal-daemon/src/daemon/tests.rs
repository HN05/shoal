use super::*;

#[tokio::test(start_paused = true)]
async fn background_loops_restart_after_a_panic() {
    let runs = std::sync::atomic::AtomicUsize::new(0);
    supervise("test loop", || async {
        if runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            panic!("failed printing to stderr");
        }
    })
    .await;
    assert_eq!(runs.into_inner(), 3);
}

#[tokio::test]
async fn reattachment_is_served_at_every_protocol_version_without_scope() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    // The spelling an older wrapper sends, with its own protocol version
    // and a scope token the restarted daemon has never issued.
    let request = serde_json::json!({
        "protocol": 1, "id": 3, "scope": "unknown",
        "method": {"reattach": {
            "execution": "missing", "wrapper": wrapper, "child": null,
            "group_id": wrapper.pid, "scope_token": "token"
        }}
    });
    let (client, stream) = UnixStream::pair().unwrap();
    let operation = manager.gates.owned_operation().await;
    let served = tokio::spawn(serve(stream, server(manager), operation));
    let mut client = BufReader::new(client);
    client
        .get_mut()
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    timeout(Duration::from_secs(60), client.read_line(&mut response))
        .await
        .unwrap()
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], 3);
    assert_eq!(response["data"]["code"], "execution_failed", "{response}");
    served.await.unwrap().unwrap();
}

#[tokio::test]
async fn an_exit_request_is_served_at_every_protocol_version_except_to_scopes() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let (exit, mut requested) = tokio::sync::mpsc::channel(1);
    let server = Server {
        exit,
        ..server(manager)
    };
    let (refused, _) = ask_exit(&server, Some("unknown"), "restart").await;
    assert_eq!(refused["data"]["code"], "scope_denied", "{refused}");
    assert!(
        requested.try_recv().is_err(),
        "a refused request ends nothing"
    );
    let (answered, _) = ask_exit(&server, None, "restart").await;
    assert_eq!(answered["id"], 4);
    assert_eq!(answered["type"], "ok", "{answered}");
    assert_eq!(requested.try_recv().unwrap().ending, Ending::Detach);
}

#[tokio::test]
async fn an_exit_answers_again_whether_it_left_stopped_work() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let (exit, mut requested) = tokio::sync::mpsc::channel(1);
    let server = Server {
        exit,
        ..server(manager.clone())
    };
    let (_, mut client) = ask_exit(&server, None, "stop").await;
    crate::execution::recovery::save_record(
        &manager.paths,
        &"workspace".into(),
        &"execution".into(),
        &crate::execution::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    requested.try_recv().unwrap().answer(&manager).await;
    let exited = read_response(&mut client).await;
    assert_eq!(exited["id"], 4);
    assert_eq!(exited["type"], "exited", "{exited}");
    assert_eq!(exited["data"]["stopped_work"], true);
}

/// Ask `server` to exit with `exit` as a service command of another
/// release would, under `scope`: its reply and the connection, which hears
/// the rest.
async fn ask_exit(
    server: &Server,
    scope: Option<&str>,
    exit: &str,
) -> (serde_json::Value, tokio::io::BufReader<UnixStream>) {
    use tokio::io::{AsyncWriteExt, BufReader};
    let request = serde_json::json!({
        "protocol": 1, "id": 4, "scope": scope, "method": {"exit": exit}
    });
    let (client, stream) = UnixStream::pair().unwrap();
    let operation = server.manager.gates.owned_operation().await;
    let served = tokio::spawn(serve(stream, server.clone(), operation));
    let mut client = BufReader::new(client);
    client
        .get_mut()
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let response = read_response(&mut client).await;
    served.await.unwrap().unwrap();
    (response, client)
}

/// The next response `client` reads.
async fn read_response(client: &mut tokio::io::BufReader<UnixStream>) -> serde_json::Value {
    use tokio::io::AsyncBufReadExt;
    let mut response = String::new();
    timeout(Duration::from_secs(60), client.read_line(&mut response))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&response).unwrap()
}

/// A server for `manager`, as the daemon runs it unmanaged.
pub(super) fn server(manager: Arc<Manager>) -> Server {
    Server {
        manager,
        started: Instant::now(),
        managed: false,
        exit: tokio::sync::mpsc::channel(1).0,
        capacity: Arc::new(connections::Capacity::new(1, 1)),
    }
}

/// The data of each frame `manager` answers `method` with before `ok`,
/// checking that each fits a frame.
pub(super) async fn serve_pages(
    manager: &Arc<Manager>,
    method: serde_json::Value,
) -> Vec<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let request =
        serde_json::json!({"protocol": crate::protocol::VERSION, "id": 5, "method": method});
    let (client, stream) = UnixStream::pair().unwrap();
    let operation = manager.gates.owned_operation().await;
    let served = tokio::spawn(serve(stream, server(manager.clone()), operation));
    let mut client = BufReader::new(client);
    client
        .get_mut()
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let mut pages = Vec::new();
    loop {
        let mut line = String::new();
        client.read_line(&mut line).await.unwrap();
        assert!(line.len() <= crate::protocol::MAX_FRAME, "{}", line.len());
        let mut response: serde_json::Value = serde_json::from_str(&line).unwrap();
        match response["type"].as_str() {
            Some("ok") => break,
            Some("error") | None => panic!("{response}"),
            Some(_) => pages.push(response["data"].take()),
        }
    }
    drop(client);
    served.await.unwrap().unwrap();
    pages
}

#[tokio::test]
async fn a_hangup_cancels_a_request_that_only_reads_state() {
    let (client, mut stream) = UnixStream::pair().unwrap();
    drop(client);
    let pending = std::future::pending::<answer::Answer>();
    let cancelled = unless_abandoned(&mut stream, "request status", true, pending);
    assert!(
        timeout(Duration::from_secs(60), cancelled)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_request_that_changes_state_finishes_after_a_hangup() {
    let (client, mut stream) = UnixStream::pair().unwrap();
    drop(client);
    let finished = async { answer::Answer::Body(protocol::Body::Ok) };
    let answer = unless_abandoned(&mut stream, "request mark_ready", false, finished).await;
    assert!(matches!(
        answer,
        Some(answer::Answer::Body(protocol::Body::Ok))
    ));
}
