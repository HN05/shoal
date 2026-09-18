//! CLI side of the daemon protocol: one request per connection, plus typed
//! helpers for the queries every command shares.
use std::{io, time::Duration};

use anyhow::{Context, Result, bail, ensure};
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
        f.write_str("daemon protocol mismatch; run `shoal setup` to update the managed daemon, or restart a foreground daemon with the installed version")
    }
}

impl std::error::Error for ProtocolMismatch {}

/// Send `method` and return the open stream with the daemon's first reply,
/// including [`Body::Error`]. Executions keep using the stream; [`call`] drops it.
pub async fn open(paths: &Paths, method: Method) -> Result<(UnixStream, Body)> {
    let mut stream = UnixStream::connect(&paths.socket).await.with_context(|| {
        format!(
            "connect to {}; run `shoal setup` or `shoal daemon start`",
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
    match body {
        Body::Error { code, message } => bail!("{code}: {message}"),
        body => Ok(body),
    }
}

/// Call the daemon and unwrap the expected [`Body`] variant.
///
/// `request!(paths, Method::InspectWorkspace { workspace }, Inspection)`
macro_rules! request {
    ($paths:expr, $method:expr, $variant:ident) => {
        match $crate::client::call($paths, $method).await? {
            $crate::protocol::Body::$variant(value) => value,
            _ => ::anyhow::bail!(
                "unexpected daemon response; expected {}",
                stringify!($variant)
            ),
        }
    };
}
pub(crate) use request;

/// The settings that apply to `target` after every layer. The global config
/// is read now, so launch defaults follow it without a daemon restart.
pub async fn settings(
    paths: &Paths,
    target: crate::protocol::ConfigTarget,
) -> Result<crate::config::Effective> {
    let layer = request!(paths, Method::LayeredConfig { target }, LayeredConfig);
    crate::config::Config::load(paths)?.effective(&layer)
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
    match call(paths, Method::Status).await {
        Ok(Body::Status(status)) => Ok(Some(status)),
        Ok(_) => bail!("unexpected status response"),
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
