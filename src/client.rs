//! CLI side of the daemon protocol: one request per connection, plus typed
//! helpers for the queries every command shares.
use std::{io, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::{
    net::UnixStream,
    time::{Instant, sleep, timeout},
};

use crate::{
    model::{Inspection, Repository, Workspace},
    paths::Paths,
    protocol::{self, Body, Method, Request, Response, Status},
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
    let seconds = if matches!(method, Method::Status | Method::Shutdown) {
        3
    } else {
        3600
    };
    let (_stream, body) = timeout(Duration::from_secs(seconds), open(paths, method))
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

mod legacy {
    /// Call the daemon and unwrap the expected [`Body`] variant.
    ///
    /// `request!(paths, Method::InspectWorkspace { workspace }, Inspection)`
    macro_rules! request {
        ($paths:expr, $method:expr, $variant:ident) => {
            match $crate::client::call($paths, $method).await? {
                $crate::protocol::Body::$variant(value) => value,
                body => return Err(body.unexpected(stringify!($variant))),
            }
        };
    }
    pub(crate) use request;
}
pub(crate) use legacy::request;

/// The settings that apply to `target` after every layer. The global config
/// is read now, so launch defaults follow it without a daemon restart.
pub async fn settings(
    paths: &Paths,
    target: crate::protocol::ConfigTarget,
) -> Result<crate::config::Effective> {
    let layers = request!(paths, Method::LayeredConfig { target }, LayeredConfig);
    let mut settings = crate::config::Config::load(paths)?.effective(&layers.resolve())?;
    if settings.issue_template.is_none() {
        let config = crate::config::Config::path(paths);
        settings.issue_template = crate::templates::read(
            config.parent().context("config has no directory")?,
            crate::templates::ISSUE_FILE,
        )?;
    }
    if settings.agent_template.is_none() {
        let config = crate::config::Config::path(paths);
        settings.agent_template = crate::templates::read(
            config.parent().context("config has no directory")?,
            crate::templates::AGENT_FILE,
        )?;
    }
    Ok(settings)
}

pub async fn inspect(paths: &Paths, workspace: String) -> Result<Inspection> {
    Ok(request!(
        paths,
        Method::InspectWorkspace { workspace },
        Inspection
    ))
}

/// Workspaces visible to this caller; the daemon filters scoped requests.
pub async fn workspaces(paths: &Paths) -> Result<Vec<Workspace>> {
    Ok(request!(paths, Method::ListWorkspaces, Workspaces))
}

pub async fn repositories(paths: &Paths) -> Result<Vec<Repository>> {
    Ok(request!(paths, Method::ListRepositories, Repositories))
}

/// `None` when no daemon is listening on the socket.
pub async fn status(paths: &Paths) -> Result<Option<Status>> {
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

/// Wait up to ten seconds for the daemon to be running (or stopped).
pub async fn wait(paths: &Paths, running: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let last_error = match status(paths).await {
            Ok(status) if status.is_some() == running => return Ok(()),
            Ok(_) => None,
            Err(error) => Some(error),
        };
        ensure!(
            Instant::now() < deadline,
            "daemon did not {} within 10 seconds{}",
            if running { "start" } else { "stop" },
            last_error.map(|e| format!(": {e:#}")).unwrap_or_default()
        );
        sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RemoteError;
    use tokio::net::UnixListener;

    async fn reply<T>(mut response: Response) -> Result<T>
    where
        T: TryFrom<Body, Error = anyhow::Error>,
    {
        let temp = tempfile::tempdir()?;
        let paths = Paths::new(Some(temp.path().to_owned()))?;
        let listener = UnixListener::bind(&paths.socket)?;
        let daemon = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = protocol::read(&mut stream).await.unwrap();
            response.id = request.id;
            protocol::write(&mut stream, &response).await.unwrap();
        });
        let result = request(&paths, Method::Status).await;
        daemon.await?;
        result
    }

    #[tokio::test]
    async fn typed_requests_extract_payloads_and_acknowledgements() {
        let status = Status {
            pid: 42,
            version: "test".into(),
            uptime_secs: 12,
            managed: false,
            unread_notifications: 3,
        };
        let status: Status = reply(Response::new(1, Body::Status(status))).await.unwrap();
        assert_eq!(status.pid, 42);
        assert_eq!(status.unread_notifications, 3);
        reply::<()>(Response::new(1, Body::Ok)).await.unwrap();
    }

    #[tokio::test]
    async fn typed_requests_report_variants_and_preserve_remote_errors() {
        let error = reply::<Status>(Response::new(1, Body::Ok))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "unexpected daemon response; expected Status, received Ok"
        );
        let error = reply::<()>(Response::new(1, Body::error("scope_denied", "denied")))
            .await
            .unwrap_err();
        let remote = error.downcast_ref::<RemoteError>().unwrap();
        assert_eq!(remote.code, "scope_denied");
        assert_eq!(remote.message, "denied");
        assert_eq!(error.to_string(), "scope_denied: denied");
    }

    #[tokio::test]
    async fn protocol_mismatch_precedes_payload_extraction() {
        let mut response = Response::new(1, Body::error("protocol_mismatch", "old daemon"));
        response.protocol += 1;
        let error = reply::<Status>(response).await.unwrap_err();
        assert!(error.is::<ProtocolMismatch>());
    }
}
