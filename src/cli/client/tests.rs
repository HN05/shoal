use super::*;
use crate::protocol::{ErrorCode, RemoteError};
use tokio::net::UnixListener;

async fn reply<T>(response: Response) -> Result<T>
where
    T: TryFrom<Body, Error = anyhow::Error>,
{
    reply_with(response, async |paths| {
        request(paths, DaemonMethod::Status).await
    })
    .await
}

async fn reply_with<T>(
    mut response: Response,
    client: impl AsyncFnOnce(&Paths) -> Result<T>,
) -> Result<T> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let daemon = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: Request = protocol::read(&mut stream).await.unwrap();
        response.id = request.id;
        protocol::write(&mut stream, &response).await.unwrap();
    });
    let result = client(&paths).await;
    daemon.await?;
    result
}

#[tokio::test(start_paused = true)]
async fn awaited_replies_survive_a_daemon_restart() -> Result<()> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let waiter = tokio::spawn({
        let paths = paths.clone();
        async move { await_reply(&paths, || DaemonMethod::Status, false).await }
    });
    // The daemon goes away mid-wait and leaves no socket behind.
    let (stream, _) = listener.accept().await?;
    drop(stream);
    drop(listener);
    std::fs::remove_file(&paths.socket)?;
    // The paused clock lets the waiter retry the missing socket meanwhile,
    // past the time a daemon not reached yet gets to start.
    sleep(timing::DAEMON_WAIT_TIMEOUT * 2).await;
    assert!(!waiter.is_finished());

    // The restarted daemon refuses the scope until the wrapper reattaches.
    let listener = UnixListener::bind(&paths.socket)?;
    let refused = Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
    for body in [refused, Body::Ok] {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        protocol::write(&mut stream, &Response::new(request.id, body)).await?;
    }
    assert!(matches!(waiter.await??, Body::Ok));

    // Without a restart, a scope refusal ends the wait at once.
    let listener_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        let body = Body::error(ErrorCode::ScopeDenied, "denied");
        protocol::write(&mut stream, &Response::new(request.id, body)).await?;
        anyhow::Ok(listener)
    });
    let error = await_reply(&paths, || DaemonMethod::Status, false)
        .await
        .unwrap_err();
    assert!(scope_denied(&error), "{error:#}");
    let listener = listener_task.await??;

    // A wait resumed after a restart retries the refusal at once.
    let resumed = tokio::spawn({
        let paths = paths.clone();
        async move { await_reply(&paths, || DaemonMethod::Status, true).await }
    });
    let refused = Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
    for body in [refused, Body::Ok] {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        protocol::write(&mut stream, &Response::new(request.id, body)).await?;
    }
    assert!(matches!(resumed.await??, Body::Ok));

    // A missing daemon ends a new wait once it had time to start, and a
    // resumed one once it had time to restart.
    drop(listener);
    std::fs::remove_file(&paths.socket)?;
    let started = Instant::now();
    let error = await_reply(&paths, || DaemonMethod::Status, false)
        .await
        .unwrap_err();
    assert!(is_unreachable(&error), "{error:#}");
    assert!(started.elapsed() < timing::DAEMON_RESTART_TIMEOUT);
    let error = await_reply(&paths, || DaemonMethod::Status, true)
        .await
        .unwrap_err();
    assert!(is_unreachable(&error), "{error:#}");
    assert!(started.elapsed() >= timing::DAEMON_RESTART_TIMEOUT);
    Ok(())
}

#[tokio::test]
async fn typed_requests_extract_payloads_and_acknowledgements() {
    let status = DaemonStatus {
        pid: 42,
        version: "test".into(),
        uptime_secs: 12,
        managed: false,
        unread_notifications: 3,
    };
    let status: DaemonStatus = reply(Response::new(1, Body::Status(status))).await.unwrap();
    assert_eq!(status.pid, 42);
    assert_eq!(status.unread_notifications, 3);
    reply::<()>(Response::new(1, Body::Ok)).await.unwrap();
}

#[tokio::test]
async fn typed_requests_report_unexpected_variants() {
    let error = reply::<DaemonStatus>(Response::new(1, Body::Ok))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "unexpected daemon response; expected Status, received Ok"
    );
}

#[tokio::test]
async fn requests_and_execution_setup_preserve_inspectable_codes() {
    for code in [
        ErrorCode::ScopeDenied,
        ErrorCode::OperationFailed,
        ErrorCode::ExecutionFailed,
        ErrorCode::ProtocolMismatch,
        ErrorCode::Unknown("future_code".into()),
    ] {
        let response = || Response::new(1, Body::error(code.clone(), "failed: detail"));
        for error in [
            reply::<DaemonStatus>(response()).await.unwrap_err(),
            reply_with(response(), async |paths| {
                crate::execution::setup(paths, "worker".into()).await
            })
            .await
            .unwrap_err(),
        ] {
            assert!(!error.is::<ProtocolMismatch>());
            let display = match code {
                ErrorCode::OperationFailed => "failed: detail".to_owned(),
                _ => format!("{code}: failed: detail"),
            };
            assert_eq!(error.to_string(), display);
            let error = error.context("caller context");
            let remote = error.downcast_ref::<RemoteError>().unwrap();
            assert_eq!(remote.code, code);
            assert_eq!(remote.message, "failed: detail");
            assert_eq!(format!("{error:#}"), format!("caller context: {display}"));
        }
    }
}

#[tokio::test]
async fn protocol_mismatch_precedes_payload_extraction() {
    for code in [
        ErrorCode::ProtocolMismatch,
        ErrorCode::Unknown("future_code".into()),
    ] {
        let response = || {
            let mut response = Response::new(1, Body::error(code.clone(), "old daemon"));
            response.protocol += 1;
            response
        };
        for error in [
            reply::<DaemonStatus>(response()).await.unwrap_err(),
            reply_with(response(), async |paths| {
                crate::execution::setup(paths, "worker".into()).await
            })
            .await
            .unwrap_err(),
        ] {
            assert!(error.is::<ProtocolMismatch>());
            assert!(!error.is::<RemoteError>());
            assert!(error.to_string().contains("shoal daemon install"));
        }
    }
}

#[tokio::test]
async fn a_daemon_that_owns_the_state_directory_without_answering_is_unresponsive() -> Result<()> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    assert!(status(&paths).await?.is_none());
    // This process stands in for a daemon whose socket refuses connections.
    let owner = crate::process::identity::capture(std::process::id())?.context("no identity")?;
    std::fs::write(paths.daemon_lock(), serde_json::to_vec(&owner)?)?;
    let error = status(&paths).await.unwrap_err();
    let unresponsive = error
        .downcast_ref::<Unresponsive>()
        .context("expected an unresponsive daemon")?;
    assert_eq!(unresponsive.owner.pid, std::process::id());
    let message = format!("{error:#}");
    assert!(message.contains("not responding"), "{message}");
    assert!(message.contains("shoal daemon restart"), "{message}");
    // Followers still wait for it as for a daemon that went away.
    assert!(is_unreachable(&error));
    Ok(())
}
