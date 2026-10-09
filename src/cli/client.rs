//! CLI side of the daemon protocol: one request per connection, plus typed
//! helpers for the queries every command shares.
use std::io;

use anyhow::{Context, Result, ensure};
use tokio::{
    io::{AsyncBufRead, BufReader},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
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
    let (reader, body) = open_buffered(paths, method).await?;
    Ok((reader.into_inner(), body))
}

/// Preserve frames read alongside the initial reply for long-lived streams.
pub async fn open_buffered(paths: &Paths, method: Method) -> Result<(BufReader<UnixStream>, Body)> {
    let mut stream = connect(paths).await?;
    let request = Request::new(method);
    protocol::write(&mut stream, &request).await?;
    let mut stream = BufReader::new(stream);
    let body = reply(&mut stream, &request).await?;
    Ok((stream, body))
}

/// [`open_buffered`] with the stream split, so a task can own the receive half.
pub async fn open_split(
    paths: &Paths,
    method: Method,
) -> Result<(BufReader<OwnedReadHalf>, OwnedWriteHalf, Body)> {
    let (reader, mut writer) = connect(paths).await?.into_split();
    let request = Request::new(method);
    protocol::write(&mut writer, &request).await?;
    let mut reader = BufReader::new(reader);
    let body = reply(&mut reader, &request).await?;
    Ok((reader, writer, body))
}

async fn connect(paths: &Paths) -> Result<UnixStream> {
    UnixStream::connect(&paths.socket).await.with_context(|| {
        format!(
            "connect to {}; run `shoal install` or `shoal daemon start`",
            paths.socket.display()
        )
    })
}

async fn reply(stream: &mut (impl AsyncBufRead + Unpin), request: &Request) -> Result<Body> {
    let reply: Response = protocol::read_buffered(stream).await?;
    if reply.protocol != protocol::VERSION {
        return Err(ProtocolMismatch.into());
    }
    ensure!(reply.id == request.id, "unexpected daemon response ID");
    Ok(reply.body)
}

/// One request; daemon errors become `code: message` failures.
pub async fn call(paths: &Paths, method: Method) -> Result<Body> {
    let duration = if matches!(
        method,
        Method::Status | Method::Shutdown | Method::ReloadConfig
    ) {
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
    Ok(configuration(paths, target).await?.1)
}

/// The global config read at launch and the settings it yields for `target`.
pub async fn configuration(
    paths: &Paths,
    target: crate::protocol::ConfigTarget,
) -> Result<(crate::config::Config, crate::config::Effective)> {
    let layers = request::<Box<ConfigLayers>>(paths, Method::LayeredConfig { target }).await?;
    let config = crate::config::Config::load_with_templates(paths)?;
    let settings = config.resolve(&layers)?;
    Ok((config, settings))
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

/// Ask a running daemon to reread the global config; `false` when none is
/// running, since a starting daemon reads it anyway.
pub async fn reload_config(paths: &Paths) -> Result<bool> {
    match call(paths, Method::ReloadConfig).await {
        Ok(_) => Ok(true),
        Err(error) if is_unreachable(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

pub(crate) fn is_unreachable(error: &anyhow::Error) -> bool {
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

pub(crate) fn stream_closed(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|e| e.downcast_ref::<io::Error>())
        .any(|e| {
            matches!(
                e.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
            )
        })
}

/// Only read-only followers reconnect; ordinary requests must never replay an
/// operation whose response was lost. The normal daemon wait budget bounds a
/// manually stopped or failed daemon while a managed update keeps its socket.
pub(crate) async fn reconnect(
    paths: &Paths,
    method: impl Fn() -> Method,
) -> Result<(BufReader<UnixStream>, Body)> {
    timeout(timing::DAEMON_WAIT_TIMEOUT, async {
        loop {
            match open_buffered(paths, method()).await {
                Ok(connection) => return Ok(connection),
                Err(error) if is_unreachable(&error) || stream_closed(&error) => {
                    sleep(timing::DAEMON_POLL_INTERVAL).await
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .context("daemon did not restart for the followed stream")?
}

/// Wait for the daemon to be running (or stopped).
pub async fn wait(paths: &Paths, running: bool) -> Result<()> {
    // A stopping daemon first stops its connected executions.
    let limit = if running {
        timing::DAEMON_WAIT_TIMEOUT
    } else {
        timing::DAEMON_WAIT_TIMEOUT + timing::WORKSPACE_STOP_TIMEOUT
    };
    let deadline = Instant::now() + limit;
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
            limit.as_secs(),
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
