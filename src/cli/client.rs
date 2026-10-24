//! CLI side of the daemon protocol: one request per connection, plus typed
//! helpers for the queries every command shares.
use std::{io, time::Duration};

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
    model::{Chat, Inspection, Repository, Workspace},
    paths::Paths,
    protocol::{self, Body, DaemonStatus, Method, Request, Response, timing},
};

/// The daemon answered with another protocol `version`.
#[derive(Debug)]
pub struct ProtocolMismatch {
    pub daemon: u32,
}

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
    let request = Request::new(method);
    let mut update = UpdateWait::default();
    loop {
        let mut stream = connect(paths).await?;
        protocol::write(&mut stream, &request).await?;
        let mut stream = BufReader::new(stream);
        match reply(&mut stream, &request).await {
            Ok(body) => return Ok((stream, body)),
            Err(error) => update.retry(paths, &request, error).await?,
        }
    }
}

/// [`open_buffered`] with the stream split, so a task can own the receive
/// half. `limit` bounds each reply, not a wait for an update.
pub async fn open_split(
    paths: &Paths,
    method: Method,
    limit: Duration,
) -> Result<(BufReader<OwnedReadHalf>, OwnedWriteHalf, Body)> {
    let request = Request::new(method);
    let mut update = UpdateWait::default();
    loop {
        let (reader, mut writer) = connect(paths).await?.into_split();
        protocol::write(&mut writer, &request).await?;
        let mut reader = BufReader::new(reader);
        let replied = timeout(limit, reply(&mut reader, &request))
            .await
            .context("daemon did not reply in time")?;
        match replied {
            Ok(body) => return Ok((reader, writer, body)),
            Err(error) => update.retry(paths, &request, error).await?,
        }
    }
}

/// A managed daemon from before the installed update refuses requests of the
/// newer protocol without running them, and restarts with the update once
/// idle, so the request is sent again until the replacement answers.
#[derive(Default)]
struct UpdateWait {
    deadline: Option<Instant>,
}

impl UpdateWait {
    /// Return after a pause to send `request` again, or fail with `error`.
    async fn retry(
        &mut self,
        paths: &Paths,
        request: &Request,
        error: anyhow::Error,
    ) -> Result<()> {
        if !awaits_update(paths, &request.method, &error) {
            return Err(error);
        }
        let deadline = *self.deadline.get_or_insert_with(|| {
            eprintln!("Waiting for the daemon to restart with the installed update...");
            Instant::now() + timing::UPDATE_WAIT_TIMEOUT
        });
        if Instant::now() >= deadline {
            return Err(error.context(format!(
                "the daemon did not restart with the installed update within {} minutes",
                timing::UPDATE_WAIT_TIMEOUT.as_secs() / 60
            )));
        }
        sleep(timing::UPDATE_POLL_INTERVAL).await;
        Ok(())
    }
}

/// Whether `error` is an older managed daemon's refusal of `method`. Daemon
/// controls report the mismatch at once, so they can replace the daemon.
fn awaits_update(paths: &Paths, method: &Method, error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ProtocolMismatch>()
        .is_some_and(|mismatch| mismatch.daemon < protocol::VERSION)
        && !method.controls_daemon()
        && crate::service::installed(paths)
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
        return Err(ProtocolMismatch {
            daemon: reply.protocol,
        }
        .into());
    }
    ensure!(reply.id == request.id, "unexpected daemon response ID");
    Ok(reply.body)
}

/// One request; daemon errors become `code: message` failures.
pub async fn call(paths: &Paths, method: Method) -> Result<Body> {
    let duration = if method.controls_daemon() {
        timing::ADMIN_REQUEST_TIMEOUT
    } else {
        timing::REQUEST_TIMEOUT
    };
    let _span = crate::profile::span("call", || method.name());
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

/// The items of every page the daemon sends for `method` before `ok`.
pub async fn pages<T>(paths: &Paths, method: Method) -> Result<Vec<T>>
where
    Vec<T>: TryFrom<Body, Error = anyhow::Error>,
{
    let _span = crate::profile::span("call", || method.name());
    let (mut reader, body) = open_buffered(paths, method).await?;
    let mut items = Vec::<T>::try_from(body)?;
    loop {
        let response: Response = protocol::read_buffered(&mut reader).await?;
        match response.body {
            Body::Ok => return Ok(items),
            body => items.extend(Vec::<T>::try_from(body)?),
        }
    }
}

/// The forge's JSON response to `request` in `workspace`'s repository,
/// reassembled from the pieces the daemon sends it in.
pub async fn forge_api(
    paths: &Paths,
    workspace: String,
    request: crate::forge::raw::RawRequest,
) -> Result<serde_json::Value> {
    let pieces = pages::<String>(paths, Method::ForgeApi { workspace, request }).await?;
    serde_json::from_str(&pieces.concat()).context("the daemon sent invalid JSON")
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

/// Record a chat whose agent replaces this process.
pub async fn start_chat(paths: &Paths, repository: &Repository, agent: &str) -> Result<Chat> {
    request(
        paths,
        Method::StartChat {
            repository: repository.id.clone().into(),
            agent: agent.to_owned(),
            pid: std::process::id(),
        },
    )
    .await
}

/// Hand `chat` to process `pid`, whose agent now runs it.
#[cfg_attr(not(feature = "happy"), allow(dead_code))]
pub async fn move_chat(paths: &Paths, chat: &Chat, pid: u32) -> Result<Chat> {
    request(
        paths,
        Method::MoveChat {
            chat: chat.id.clone(),
            pid,
        },
    )
    .await
}

/// Running chats; listing them ends those whose agent exited.
pub async fn chats(paths: &Paths) -> Result<Vec<Chat>> {
    request(paths, Method::ListChats).await
}

pub async fn repositories(paths: &Paths) -> Result<Vec<Repository>> {
    request(paths, Method::ListRepositories).await
}

/// Keep where an agent of `workspace` keeps its configuration for the
/// repository's removal.
pub async fn keep_agent_environment(
    paths: &Paths,
    workspace: &str,
    environment: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    call(
        paths,
        Method::KeepAgentEnvironment {
            workspace: workspace.to_owned(),
            environment,
        },
    )
    .await?;
    Ok(())
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

/// The reply to a long-lived request that waits for an event, across daemon
/// restarts: a closed connection asks again at once, and an unreachable socket
/// is retried for the normal daemon wait budget, or the restart budget once
/// the daemon went away. After a restart, a scope refusal is retried within
/// the restart budget too, since a restarted daemon knows an execution's scope
/// only once its wrapper reattaches; `restarted` says the daemon restarted
/// before this wait began. Only for requests that are safe to repeat.
pub(crate) async fn await_reply(
    paths: &Paths,
    method: impl Fn() -> Method,
    restarted: bool,
) -> Result<Body> {
    let mut outage = restarted.then(Outage::restart);
    loop {
        let reply = open(paths, method()).await;
        match reply.and_then(|(_stream, body)| body.into_result()) {
            Ok(body) => return Ok(body),
            Err(error) if stream_closed(&error) => outage = Some(Outage::restart()),
            Err(error) if is_unreachable(&error) || (outage.is_some() && scope_denied(&error)) => {
                if outage.get_or_insert_with(Outage::start).over() {
                    return Err(error.context("daemon did not restart"));
                }
            }
            Err(error) => return Err(error),
        }
        sleep(timing::DAEMON_POLL_INTERVAL).await;
    }
}

/// A wait's time without a daemon to answer it.
struct Outage {
    /// When the daemon last went away, or the socket was first found unreachable.
    since: Instant,
    limit: Duration,
}

impl Outage {
    /// A daemon that answered before gets time to restart and reattach wrappers.
    fn restart() -> Self {
        Self {
            since: Instant::now(),
            limit: timing::DAEMON_RESTART_TIMEOUT,
        }
    }

    /// A daemon not reached yet gets the normal time to start.
    fn start() -> Self {
        Self {
            since: Instant::now(),
            limit: timing::DAEMON_WAIT_TIMEOUT,
        }
    }

    fn over(&self) -> bool {
        self.since.elapsed() >= self.limit
    }
}

fn scope_denied(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<protocol::RemoteError>()
        .is_some_and(|remote| remote.code == protocol::ErrorCode::ScopeDenied)
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

    #[tokio::test(start_paused = true)]
    async fn awaited_replies_survive_a_daemon_restart() -> Result<()> {
        let temp = tempfile::tempdir_in("/tmp")?;
        let paths = Paths::for_test(temp.path());
        paths.prepare()?;
        let listener = UnixListener::bind(&paths.socket)?;
        let waiter = tokio::spawn({
            let paths = paths.clone();
            async move { await_reply(&paths, || Method::Status, false).await }
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
        let error = await_reply(&paths, || Method::Status, false)
            .await
            .unwrap_err();
        assert!(scope_denied(&error), "{error:#}");
        let listener = listener_task.await??;

        // A wait resumed after a restart retries the refusal at once.
        let resumed = tokio::spawn({
            let paths = paths.clone();
            async move { await_reply(&paths, || Method::Status, true).await }
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
        let error = await_reply(&paths, || Method::Status, false)
            .await
            .unwrap_err();
        assert!(is_unreachable(&error), "{error:#}");
        assert!(started.elapsed() < timing::DAEMON_RESTART_TIMEOUT);
        let error = await_reply(&paths, || Method::Status, true)
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
