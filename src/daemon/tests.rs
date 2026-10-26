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
async fn an_oversized_reply_is_answered_with_an_error_saying_so() {
    let (mut writer, reader) = tokio::io::duplex(4 * protocol::MAX_FRAME);
    let env = [("VALUE".to_owned(), "x".repeat(protocol::MAX_FRAME))].into();
    let error = reply(&mut writer, 7, Body::WorkspaceEnv(env))
        .await
        .unwrap_err();
    assert!(error.is::<protocol::FrameTooLarge>(), "{error:#}");
    drop(writer);
    let response: Response = protocol::read(&mut tokio::io::BufReader::new(reader))
        .await
        .unwrap();
    assert_eq!(response.id, 7);
    let Body::Error { code, message } = response.body else {
        panic!("expected an error reply");
    };
    assert_eq!(code, ErrorCode::OperationFailed);
    assert!(
        message.contains("exceeds the 65536-byte protocol frame"),
        "{message}"
    );
}

#[tokio::test]
async fn item_waits_close_unanswered_while_an_update_restarts_the_daemon() {
    use tokio::io::AsyncReadExt;
    let (root, manager) = crate::test_support::manager().await;
    let path = crate::test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "wait".into(), None, None, None)
        .await
        .unwrap();
    manager
        .send_agent_message(&workspace.id, "Rebase onto main".into())
        .await
        .unwrap();
    let wait = |manager: Arc<Manager>| {
        let (client, stream) = UnixStream::pair().unwrap();
        let served = tokio::spawn(wait_pr_updates(
            stream,
            5,
            manager,
            workspace.id.clone(),
            Some(60),
        ));
        (client, served)
    };
    let update = manager.background_operations.clone().write_owned().await;
    let (mut client, served) = wait(manager.clone());
    served.await.unwrap().unwrap();
    let mut sent = Vec::new();
    client.read_to_end(&mut sent).await.unwrap();
    assert!(sent.is_empty(), "{}", String::from_utf8_lossy(&sent));
    drop(update);
    // The message stays pending for the wait on the replacement daemon.
    let (client, served) = wait(manager);
    served.await.unwrap().unwrap();
    let response: Response = protocol::read(&mut tokio::io::BufReader::new(client))
        .await
        .unwrap();
    let Body::PrUpdates(updates) = response.body else {
        panic!("expected item updates");
    };
    assert_eq!(updates.messages[0].message, "Rebase onto main");
}

#[tokio::test]
async fn reattachment_is_served_at_every_protocol_version_without_scope() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (_root, manager) = crate::test_support::manager().await;
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
    let operation = manager.background_operations.clone().read_owned().await;
    let capacity = Arc::new(connections::Capacity::new(1, 1));
    let server = Server {
        manager,
        started: Instant::now(),
        managed: false,
        shutdown: watch::channel(false).0,
        capacity: capacity.clone(),
    };
    let served = tokio::spawn(serve(stream, server, capacity.request_slot(), operation));
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
    assert!(
        response["data"]["message"]
            .as_str()
            .unwrap()
            .contains("execution record is missing"),
        "{response}"
    );
    served.await.unwrap().unwrap();
}
