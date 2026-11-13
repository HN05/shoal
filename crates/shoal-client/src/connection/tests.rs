use super::*;
use crate::protocol::{ErrorCode, RemoteError, WorkPlace};
use tokio::net::UnixListener;

/// Supervision that takes the daemon recorded in the state directory's
/// owner file for its owner, as the `shoal` command's does for a live one.
struct RecordedOwner;

impl Supervision for RecordedOwner {
    fn owner(&self, paths: &Paths) -> Option<Identity> {
        serde_json::from_slice(&std::fs::read(owner_file(paths)).ok()?).ok()
    }

    fn managed(&self, _paths: &Paths) -> bool {
        false
    }

    fn waiting_for_update(&self) {}

    fn waiting_for_restart(&self) {}
}

/// Where [`RecordedOwner`] reads the daemon that owns the state directory.
fn owner_file(paths: &Paths) -> std::path::PathBuf {
    paths.state.join("owner")
}

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

#[tokio::test(start_paused = true)]
async fn requests_after_a_restart_wait_for_the_scope() -> Result<()> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let asking = tokio::spawn({
        let paths = paths.clone();
        async move { after_restart(|| request::<()>(&paths, DaemonMethod::Status)).await }
    });
    // The restarted daemon refuses the scope until the wrapper reattaches.
    let refused = Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
    for body in [refused, Body::Ok] {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        protocol::write(&mut stream, &Response::new(request.id, body)).await?;
    }
    asking.await??;

    // Another refusal ends it at once.
    let listener_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        let body = Body::error(ErrorCode::NotFound, "no such workspace");
        protocol::write(&mut stream, &Response::new(request.id, body)).await?;
        anyhow::Ok(listener)
    });
    let error = after_restart(|| request::<()>(&paths, DaemonMethod::Status))
        .await
        .unwrap_err();
    assert!(!scope_denied(&error), "{error:#}");
    let listener = listener_task.await??;

    // A scope the daemon never knows again ends it once a restart had time.
    let refusing = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let Ok(request) = protocol::read::<Request>(&mut stream).await else {
                return;
            };
            let body = Body::error(ErrorCode::ScopeDenied, "denied");
            let _ = protocol::write(&mut stream, &Response::new(request.id, body)).await;
        }
    });
    let started = Instant::now();
    let error = after_restart(|| request::<()>(&paths, DaemonMethod::Status))
        .await
        .unwrap_err();
    assert!(scope_denied(&error), "{error:#}");
    assert!(started.elapsed() >= timing::DAEMON_RESTART_TIMEOUT);
    refusing.abort();
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
        access_stalls: Vec::new(),
        cleanup_error: None,
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
async fn requests_preserve_inspectable_codes() {
    for code in [
        ErrorCode::ScopeDenied,
        ErrorCode::OperationFailed,
        ErrorCode::ExecutionFailed,
        ErrorCode::ProtocolMismatch,
        ErrorCode::Unknown("future_code".into()),
    ] {
        let response = || Response::new(1, Body::error(code.clone(), "failed: detail"));
        {
            let error = reply::<DaemonStatus>(response()).await.unwrap_err();
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
        {
            let error = reply::<DaemonStatus>(response()).await.unwrap_err();
            assert!(error.is::<ProtocolMismatch>());
            assert!(!error.is::<RemoteError>());
        }
    }
}

#[tokio::test]
async fn a_daemon_that_owns_the_state_directory_without_answering_is_unresponsive() -> Result<()> {
    // The shoal command tells an unresponsive daemon apart; apps do not.
    supervise(&RecordedOwner);
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    assert!(status(&paths).await?.is_none());
    // This process stands in for a daemon whose socket refuses connections.
    let owner = Identity {
        pid: std::process::id(),
        birth: "test".into(),
        unique: None,
    };
    std::fs::write(owner_file(&paths), serde_json::to_vec(&owner)?)?;
    let error = status(&paths).await.unwrap_err();
    let unresponsive = error
        .downcast_ref::<Unresponsive>()
        .context("expected an unresponsive daemon")?;
    assert_eq!(unresponsive.owner.pid, std::process::id());
    // Followers still wait for it as for a daemon that went away.
    assert!(is_unreachable(&error));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_split_request_waits_through_keepalives_until_the_daemon_goes_quiet() -> Result<()> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let limit = Duration::from_secs(30);
    // Keepalives within the limit outlast it many times over.
    let daemon = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        for _ in 0..10 {
            sleep(limit / 2).await;
            protocol::write(&mut stream, &Response::new(request.id, Body::Keepalive)).await?;
        }
        protocol::write(&mut stream, &Response::new(request.id, Body::Ok)).await?;
        // Then it falls silent on the next request.
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        protocol::write(&mut stream, &Response::new(request.id, Body::Keepalive)).await?;
        anyhow::Ok(stream)
    });
    let started = Instant::now();
    let (_, _, body) = open_split(&paths, DaemonMethod::Status, limit).await?;
    assert!(matches!(body, Body::Ok), "{body:?}");
    assert!(started.elapsed() >= limit * 5);

    let started = Instant::now();
    let Err(error) = open_split(&paths, DaemonMethod::Status, limit).await else {
        panic!("a silent daemon answered");
    };
    assert!(started.elapsed() >= limit);
    assert!(format!("{error:#}").contains("sent nothing"), "{error:#}");
    daemon.await??;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_request_an_exiting_daemon_held_back_goes_to_the_next_daemon() -> Result<()> {
    supervise(&RecordedOwner);
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    own(&paths, "exiting")?;
    let listener = UnixListener::bind(&paths.socket)?;
    let cancel = || crate::protocol::OperationMethod::CancelOperation { id: "op".into() };
    let asking = tokio::spawn({
        let paths = paths.clone();
        async move { request::<()>(&paths, cancel()).await }
    });
    let exiting = Body::error(ErrorCode::Exiting, "the daemon is about to exit");
    let (mut stream, _) = listener.accept().await?;
    let request: Request = protocol::read(&mut stream).await?;
    protocol::write(&mut stream, &Response::new(request.id, exiting)).await?;
    // While it exits, the daemon still takes connections it never answers.
    drop(stream);
    let (unanswered, _) = listener.accept().await?;
    drop(unanswered);
    // Then it leaves no socket until the next one listens.
    drop(listener);
    std::fs::remove_file(&paths.socket)?;
    sleep(timing::DAEMON_WAIT_TIMEOUT * 2).await;
    assert!(!asking.is_finished());
    let listener = UnixListener::bind(&paths.socket)?;
    let (mut stream, _) = listener.accept().await?;
    let request: Request = protocol::read(&mut stream).await?;
    protocol::write(&mut stream, &Response::new(request.id, Body::Ok)).await?;
    asking.await??;

    // A daemon control hears of the exit instead of waiting for it.
    let answering = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        let exiting = Body::error(ErrorCode::Exiting, "the daemon is about to exit");
        protocol::write(&mut stream, &Response::new(request.id, exiting)).await
    });
    let error = call(&paths, DaemonMethod::Status).await.unwrap_err();
    let remote = error.downcast_ref::<RemoteError>().unwrap();
    assert_eq!(remote.code, ErrorCode::Exiting);
    answering.await??;
    Ok(())
}

/// How a daemon's exit ended in [`exit_answering`].
#[derive(Debug, PartialEq)]
enum Ended {
    Exited {
        waited_for: Vec<Work>,
        stopped_work: Option<bool>,
    },
    Refused(Vec<Work>),
    Gone(Vec<Work>),
}

/// Ask a daemon to exit as `when` says, while it answers with `bodies`:
/// how the exit ended, and the work `exit` reported waiting for.
async fn exit_answering(when: ExitWhen, bodies: Vec<Body>) -> Result<(Ended, Vec<Vec<Work>>)> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let daemon = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        let Method::Exit(exit) = &request.method else {
            anyhow::bail!("expected an exit request");
        };
        anyhow::ensure!(exit.when == when);
        for body in bodies {
            protocol::write(&mut stream, &Response::new(request.id, body)).await?;
        }
        anyhow::Ok(())
    });
    let mut reported = Vec::new();
    let request = ExitRequest {
        exit: crate::protocol::Exit::Restart,
        when,
    };
    let ended = match exit(&paths, request, |work| reported.push(work.to_vec())).await? {
        ExitAnswer::Exiting(exiting) => Ended::Exited {
            waited_for: exiting.waited_for.clone(),
            stopped_work: exiting.stopped_work().await,
        },
        ExitAnswer::Refused(work) => Ended::Refused(work),
        ExitAnswer::Gone(work) => Ended::Gone(work),
        ExitAnswer::Unanswered => anyhow::bail!("the daemon answered"),
    };
    daemon.await??;
    Ok((ended, reported))
}

#[tokio::test]
async fn an_exit_that_waits_reports_the_work_until_the_daemon_agrees() -> Result<()> {
    let exited = Body::Exited(crate::protocol::Exited {
        stopped_work: false,
    });
    let waits = || Body::ExitWaits(vec![Work::Other]);
    let bodies = vec![waits(), waits(), Body::Ok, exited];
    let (ended, reported) = exit_answering(ExitWhen::Idle, bodies).await?;
    let exited = Ended::Exited {
        waited_for: vec![Work::Other],
        stopped_work: Some(false),
    };
    assert_eq!(ended, exited);
    assert_eq!(reported, [vec![Work::Other], vec![Work::Other]]);
    Ok(())
}

#[tokio::test]
async fn an_exit_that_does_not_wait_reports_the_work_that_keeps_the_daemon() -> Result<()> {
    let workspace = WorkPlace {
        id: "workspace".into(),
        number: 3.into(),
        name: "api".parse().unwrap(),
    };
    let named = Work::Command { workspace };
    let bodies = vec![Body::ExitWaits(vec![named.clone()])];
    let (ended, reported) = exit_answering(ExitWhen::IfIdle, bodies).await?;
    assert_eq!(ended, Ended::Refused(vec![named]));
    assert!(reported.is_empty());
    Ok(())
}

#[tokio::test]
async fn an_exit_that_does_not_wait_still_waits_out_requests_and_passes() -> Result<()> {
    let exited = Body::Exited(crate::protocol::Exited {
        stopped_work: false,
    });
    let bodies = vec![Body::ExitWaits(vec![Work::Other]), Body::Ok, exited];
    let (ended, reported) = exit_answering(ExitWhen::IfIdle, bodies).await?;
    let exited = Ended::Exited {
        waited_for: vec![Work::Other],
        stopped_work: Some(false),
    };
    assert_eq!(ended, exited);
    assert_eq!(reported, [vec![Work::Other]]);
    Ok(())
}

#[tokio::test]
async fn a_daemon_that_goes_while_its_exit_waits_is_told_apart_from_one_that_never_answered()
-> Result<()> {
    let bodies = vec![Body::ExitWaits(vec![Work::Other])];
    let (ended, reported) = exit_answering(ExitWhen::Idle, bodies).await?;
    assert_eq!(ended, Ended::Gone(vec![Work::Other]));
    assert_eq!(reported, [vec![Work::Other]]);
    Ok(())
}

/// Record this process, born at `birth`, as the daemon that owns the state
/// directory of `paths`, as [`RecordedOwner`] reads it.
fn own(paths: &Paths, birth: &str) -> Result<()> {
    let owner = Identity {
        pid: std::process::id(),
        birth: birth.into(),
        unique: None,
    };
    Ok(std::fs::write(
        owner_file(paths),
        serde_json::to_vec(&owner)?,
    )?)
}

#[tokio::test(start_paused = true)]
async fn a_held_back_request_another_daemon_took_is_not_sent_again() -> Result<()> {
    supervise(&RecordedOwner);
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    own(&paths, "exiting")?;
    let listener = UnixListener::bind(&paths.socket)?;
    let cancel = || crate::protocol::OperationMethod::CancelOperation { id: "op".into() };
    let asking = tokio::spawn({
        let paths = paths.clone();
        async move { request::<()>(&paths, cancel()).await }
    });
    let exiting = Body::error(ErrorCode::Exiting, "the daemon is about to exit");
    let (mut stream, _) = listener.accept().await?;
    let request: Request = protocol::read(&mut stream).await?;
    protocol::write(&mut stream, &Response::new(request.id, exiting)).await?;
    drop(stream);
    // The paused clock moves only once the client waits to send it again,
    // having read the refusal.
    sleep(timing::UPDATE_POLL_INTERVAL / 2).await;
    // The next daemon takes the request and goes before it answers, so it
    // may have run it.
    own(&paths, "next")?;
    let (mut taken, _) = listener.accept().await?;
    let _: Request = protocol::read(&mut taken).await?;
    drop(taken);
    let error = asking.await?.unwrap_err();
    assert!(stream_closed(&error), "{error:#}");
    Ok(())
}
