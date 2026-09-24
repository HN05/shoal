//! CLI side of the daemon protocol: one request per connection, plus typed
//! helpers for the queries every command shares.
use std::io;

use anyhow::{Context, Result, ensure};
use tokio::{
    net::UnixStream,
    time::{Instant, sleep, timeout},
};

use crate::{
    config::repo::ConfigLayers,
    model::{Inspection, Repository, Workspace},
    paths::Paths,
    protocol::{self, Body, DaemonStatus, Method, Request, Response, timing},
};

#[derive(Debug)]
pub struct ProtocolMismatch;

impl std::fmt::Display for ProtocolMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("daemon protocol mismatch; run `shoal install` to update the managed daemon, or restart a foreground daemon with the installed version")
    }
}

impl std::error::Error for ProtocolMismatch {}

/// Send `method` and return the open stream with the daemon's first reply,
/// including [`Body::Error`]. Executions keep using the stream; [`call`] drops it.
pub async fn open(paths: &Paths, method: Method) -> Result<(UnixStream, Body)> {
    let mut stream = UnixStream::connect(&paths.socket).await.with_context(|| {
        format!(
            "connect to {}; run `shoal install` or `shoal daemon start`",
            paths.socket.display()
        )
    })?;
    let request = Request::new(method);
    protocol::write(&mut stream, &request).await?;
    let reply: Response = protocol::read(&mut stream).await?;
    if reply.protocol != protocol::VERSION {
        return Err(ProtocolMismatch.into());
    }
    ensure!(reply.id == request.id, "unexpected daemon response ID");
    Ok((stream, reply.body))
}

/// One request; daemon errors become `code: message` failures.
pub async fn call(paths: &Paths, method: Method) -> Result<Body> {
    let duration = if matches!(method, Method::Status | Method::Shutdown) {
        timing::ADMIN_REQUEST_TIMEOUT
    } else {
        timing::REQUEST_TIMEOUT
    };
    let (_stream, body) = timeout(duration, open(paths, method))
        .await
        .context("daemon request timed out")??;
    body.into_result()
}

/// Send a request and extract its expected response payload.
pub async fn request<T>(paths: &Paths, method: Method) -> Result<T>
where
    T: TryFrom<Body, Error = anyhow::Error>,
{
    T::try_from(call(paths, method).await?)
}

/// The settings that apply to `target` after every layer. The global config
/// is read now, so launch defaults follow it without a daemon restart.
pub async fn settings(
    paths: &Paths,
    target: crate::protocol::ConfigTarget,
) -> Result<crate::config::Effective> {
    let layers = request::<Box<ConfigLayers>>(paths, Method::LayeredConfig { target }).await?;
    crate::config::Config::load(paths)?.resolve(&layers)
}

pub async fn inspect(paths: &Paths, workspace: String) -> Result<Inspection> {
    request(paths, Method::InspectWorkspace { workspace }).await
}

/// Workspaces visible to this caller; the daemon filters scoped requests.
pub async fn workspaces(paths: &Paths) -> Result<Vec<Workspace>> {
    request(paths, Method::ListWorkspaces).await
}

pub async fn repositories(paths: &Paths) -> Result<Vec<Repository>> {
    request(paths, Method::ListRepositories).await
}

/// `None` when no daemon is listening on the socket.
pub async fn status(paths: &Paths) -> Result<Option<DaemonStatus>> {
    match request(paths, Method::Status).await {
        Ok(status) => Ok(Some(status)),
        Err(error) if is_unreachable(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn is_unreachable(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|e| e.downcast_ref::<io::Error>())
        .any(|e| {
            matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            )
        })
}

/// Wait for the daemon to be running (or stopped).
pub async fn wait(paths: &Paths, running: bool) -> Result<()> {
    let deadline = Instant::now() + timing::DAEMON_WAIT_TIMEOUT;
    loop {
        let last_error = match status(paths).await {
            Ok(status) if status.is_some() == running => return Ok(()),
            Ok(_) => None,
            Err(error) => Some(error),
        };
        ensure!(
            Instant::now() < deadline,
            "daemon did not {} within {} seconds{}",
            if running { "start" } else { "stop" },
            timing::DAEMON_WAIT_TIMEOUT.as_secs(),
            last_error.map(|e| format!(": {e:#}")).unwrap_or_default()
        );
        sleep(timing::DAEMON_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ErrorCode, RemoteError};
    use tokio::net::UnixListener;

    async fn reply<T>(response: Response) -> Result<T>
    where
        T: TryFrom<Body, Error = anyhow::Error>,
    {
        reply_with(response, async |paths| request(paths, Method::Status).await).await
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
                    crate::execution::setup(paths, "worker".into(), false).await
                })
                .await
                .unwrap_err(),
            ] {
                assert!(!error.is::<ProtocolMismatch>());
                assert_eq!(error.to_string(), format!("{code}: failed: detail"));
                let error = error.context("caller context");
                let remote = error.downcast_ref::<RemoteError>().unwrap();
                assert_eq!(remote.code, code);
                assert_eq!(remote.message, "failed: detail");
                assert_eq!(
                    format!("{error:#}"),
                    format!("caller context: {code}: failed: detail")
                );
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
                    crate::execution::setup(paths, "worker".into(), false).await
                })
                .await
                .unwrap_err(),
            ] {
                assert!(error.is::<ProtocolMismatch>());
                assert!(!error.is::<RemoteError>());
                assert!(error.to_string().contains("shoal install"));
            }
        }
    }
}
