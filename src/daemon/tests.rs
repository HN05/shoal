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
    let operation = manager.gates.owned_operation().await;
    let server = server(manager);
    let capacity = server.capacity.clone();
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

/// A server for `manager`, as the daemon runs it unmanaged.
pub(super) fn server(manager: Arc<Manager>) -> Server {
    Server {
        manager,
        started: Instant::now(),
        managed: false,
        shutdown: watch::channel(false).0,
        capacity: Arc::new(connections::Capacity::new(1, 1)),
    }
}
