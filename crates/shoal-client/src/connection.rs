//! The client side of the daemon protocol, which the CLI and apps share:
//! finding the daemon, sending one request per connection with the caller's
//! scope token, reading its reply, and the daemon's own status, reload, exit
//! and wait.
use std::{io, sync::OnceLock, time::Duration};

use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;
use tokio::{
    io::{AsyncBufRead, BufReader},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    time::{Instant, sleep, timeout},
};

use crate::{
    paths::Paths,
    processes::Identity,
    protocol::{
        self, Body, DaemonMethod, DaemonStatus, ErrorCode, ExitRequest, ExitWhen, Method, Request,
        Response, ViewPiece, Work, timing,
    },
    scope_tokens::ScopeToken,
};

/// A daemon to send requests to, and the scope they carry.
pub trait Daemon: Sync {
    /// Where the daemon listens and keeps its state.
    fn paths(&self) -> &Paths;
    /// The scope token requests carry: with one, the caller is the agent of
    /// the workspace it belongs to; without one, the user.
    fn scope(&self) -> Option<ScopeToken>;
}

/// Tests' daemon, which they reach as the user.
#[cfg(test)]
impl Daemon for Paths {
    fn paths(&self) -> &Paths {
        self
    }

    fn scope(&self) -> Option<ScopeToken> {
        None
    }
}

/// The daemon an app connects to, with the scope it chose.
#[derive(Debug, Clone)]
pub struct Connector {
    pub paths: Paths,
    pub scope: Option<ScopeToken>,
}

impl Connector {
    /// The daemon `shoal` uses: in the state directory `SHOAL_STATE_DIR`
    /// names, else the default one, with no scope token.
    pub fn new() -> Result<Self> {
        let state = std::env::var_os(crate::env::STATE_DIR).map(Into::into);
        Ok(Self {
            paths: Paths::new(state)?,
            scope: None,
        })
    }

    /// Requests as the agent of the workspace `token` belongs to.
    pub fn scope(self, token: ScopeToken) -> Self {
        Self {
            scope: Some(token),
            ..self
        }
    }
}

impl Daemon for Connector {
    fn paths(&self) -> &Paths {
        &self.paths
    }

    fn scope(&self) -> Option<ScopeToken> {
        self.scope.clone()
    }
}

/// What the `shoal` command adds to the connections it makes: telling a
/// daemon that stopped answering from one that is not running, and waiting
/// for a managed daemon to restart with an installed update. Apps connect
/// without it.
pub trait Supervision: Send + Sync {
    /// The daemon that owns the state directory of `paths`, when one does.
    fn owner(&self, paths: &Paths) -> Option<Identity>;
    /// Whether a service manager restarts the daemon of `paths`.
    fn managed(&self, paths: &Paths) -> bool;
    /// Tell the user a request waits for the daemon's restart with an
    /// installed update.
    fn waiting_for_update(&self);
    /// Tell the user a daemon about to exit held a request back until the
    /// next one answers.
    fn waiting_for_restart(&self);
}

static SUPERVISION: OnceLock<&'static dyn Supervision> = OnceLock::new();

/// Supervise this process's connections with `supervision`.
pub fn supervise(supervision: &'static dyn Supervision) {
    let _ = SUPERVISION.set(supervision);
}

/// The daemon answered with another protocol `version`.
#[derive(Debug)]
pub struct ProtocolMismatch {
    pub daemon: u32,
}

impl std::fmt::Display for ProtocolMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "daemon protocol mismatch: the daemon speaks protocol {}, this Shoal {}; run `shoal daemon install` to update the managed daemon, or restart a foreground daemon with the installed version",
            self.daemon,
            protocol::VERSION
        )
    }
}

impl std::error::Error for ProtocolMismatch {}

/// A daemon owns the state directory but does not answer, as when it stopped
/// accepting connections.
#[derive(Debug)]
pub struct Unresponsive {
    pub owner: Identity,
}

impl std::fmt::Display for Unresponsive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the daemon (PID {}) is running but not responding; restart it with `shoal daemon restart`",
            self.owner.pid
        )
    }
}

/// [`Unresponsive`] when a live daemon owns the state directory.
fn unresponsive(paths: &Paths) -> Option<Unresponsive> {
    let owner = SUPERVISION.get()?.owner(paths)?;
    Some(Unresponsive { owner })
}

/// `method` as a request from `daemon`'s caller.
fn request_to(daemon: &impl Daemon, method: impl Into<Method>) -> Request {
    Request {
        scope: daemon.scope(),
        ..Request::new(method)
    }
}

/// Send `method` and return the open stream with the daemon's first reply,
/// including [`Body::Error`]. Executions keep using the stream; [`call`] drops it.
pub async fn open(daemon: &impl Daemon, method: impl Into<Method>) -> Result<(UnixStream, Body)> {
    let (reader, body) = open_buffered(daemon, method).await?;
    Ok((reader.into_inner(), body))
}

/// Preserve frames read alongside the initial reply for long-lived streams.
pub async fn open_buffered(
    daemon: &impl Daemon,
    method: impl Into<Method>,
) -> Result<(BufReader<UnixStream>, Body)> {
    let request = request_to(daemon, method);
    let mut waiting = DaemonWait::default();
    loop {
        let mut reached = None;
        let sent = async {
            let mut stream = connect(daemon.paths()).await?;
            reached = waiting.reached(daemon.paths());
            protocol::write(&mut stream, &request).await?;
            let mut stream = BufReader::new(stream);
            let body = reply(&mut stream, &request).await?;
            Ok((stream, body))
        };
        match sent.await {
            Ok((stream, body)) => match waiting.held_back(daemon.paths(), &request, body) {
                Some(body) => return Ok((stream, body)),
                None => waiting.pause().await?,
            },
            Err(error) => {
                (waiting.retry(daemon.paths(), &request, error, reached)).await?;
            }
        }
    }
}

/// [`open_buffered`] with the stream split, so a task can own the receive
/// half. `limit` bounds the daemon's silence before each frame of its reply,
/// not a wait for an update.
pub async fn open_split(
    daemon: &impl Daemon,
    method: impl Into<Method>,
    limit: Duration,
) -> Result<(BufReader<OwnedReadHalf>, OwnedWriteHalf, Body)> {
    let request = request_to(daemon, method);
    let mut waiting = DaemonWait::default();
    loop {
        let mut reached = None;
        let sent = async {
            let (reader, mut writer) = connect(daemon.paths()).await?.into_split();
            reached = waiting.reached(daemon.paths());
            protocol::write(&mut writer, &request).await?;
            let mut reader = BufReader::new(reader);
            let body = reply_kept_alive(&mut reader, &request, limit).await?;
            Ok((reader, writer, body))
        };
        // Only a reply, not the daemon's silence, is a refusal to retry.
        let sent: Result<_> = match sent.await {
            Ok((reader, writer, Ok(body))) => Ok((reader, writer, body)),
            Ok((_, _, Err(error))) => Err(error),
            Err(error) if waiting.gap(&error, reached.as_ref()) => Err(error),
            Err(error) => return Err(error),
        };
        match sent {
            Ok((reader, writer, body)) => match waiting.held_back(daemon.paths(), &request, body) {
                Some(body) => return Ok((reader, writer, body)),
                None => waiting.pause().await?,
            },
            Err(error) => {
                (waiting.retry(daemon.paths(), &request, error, reached)).await?;
            }
        }
    }
}

/// The daemon's reply to `request` after the keepalives it sends while it
/// still prepares it, failing once it sends nothing for `limit`.
async fn reply_kept_alive(
    stream: &mut (impl AsyncBufRead + Unpin),
    request: &Request,
    limit: Duration,
) -> Result<Result<Body>> {
    loop {
        let replied = timeout(limit, reply(stream, request))
            .await
            .with_context(|| format!("the daemon sent nothing for {} seconds", limit.as_secs()))?;
        if !matches!(replied, Ok(Body::Keepalive)) {
            return Ok(replied);
        }
    }
}

/// What a request waits for before it is sent again: a managed daemon from
/// before the installed update refuses requests of the newer protocol
/// without running them, and restarts with the update once idle, and a
/// daemon about to exit holds back requests that would start work until the
/// next one answers. Either way the request is sent again until a daemon
/// takes it.
#[derive(Default)]
struct DaemonWait {
    deadline: Option<Instant>,
    /// A daemon held the request back for its exit, so the next daemon may
    /// not listen yet.
    exiting: bool,
    /// The daemon that held it back, when supervision tells.
    held_by: Option<Identity>,
}

impl DaemonWait {
    /// `body`, unless it says a daemon about to exit held `request` back,
    /// which is then sent again, except a daemon control, which reports the
    /// daemon that runs.
    fn held_back(&mut self, paths: &Paths, request: &Request, body: Body) -> Option<Body> {
        let held_back = matches!(
            &body,
            Body::Error {
                code: ErrorCode::Exiting,
                ..
            }
        );
        if !held_back || request.method.controls_daemon() {
            return Some(body);
        }
        // A later daemon may hold it back too, as across two restarts.
        let supervision = SUPERVISION.get();
        self.held_by = supervision.and_then(|supervision| supervision.owner(paths));
        if !self.exiting {
            self.exiting = true;
            if let Some(supervision) = supervision {
                supervision.waiting_for_restart();
            }
        }
        None
    }

    /// The daemon an attempt connected to, once one held the request back.
    fn reached(&self, paths: &Paths) -> Option<Identity> {
        let supervision = SUPERVISION.get().filter(|_| self.exiting)?;
        supervision.owner(paths)
    }

    /// Whether `error` of an attempt that connected to `reached` is the gap
    /// before the next daemon after a daemon held the request back: no
    /// socket, or a connection the daemon that held it back closed
    /// unanswered as it exits, which ran none of it. Another daemon that
    /// closed it may have run it, so it is not sent again.
    fn gap(&self, error: &anyhow::Error, reached: Option<&Identity>) -> bool {
        let by_holder = reached.is_some() && reached == self.held_by.as_ref();
        self.exiting && (is_unreachable(error) || (by_holder && stream_closed(error)))
    }

    /// Return after a pause to send `request` again, or fail with `error`.
    /// An attempt that fails after connecting to `reached` waits for the
    /// next daemon while that is [`Self::gap`].
    async fn retry(
        &mut self,
        paths: &Paths,
        request: &Request,
        error: anyhow::Error,
        reached: Option<Identity>,
    ) -> Result<()> {
        if self.gap(&error, reached.as_ref()) {
            return self.pause().await.map_err(|timeout| error.context(timeout));
        }
        if !awaits_update(paths, &request.method, &error) {
            return Err(error);
        }
        let deadline = *self.deadline.get_or_insert_with(|| {
            if let Some(supervision) = SUPERVISION.get() {
                supervision.waiting_for_update();
            }
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

    /// Pause before sending a request a daemon held back for its exit
    /// again, failing once the next daemon has not taken it in time.
    async fn pause(&mut self) -> Result<()> {
        let deadline =
            *(self.deadline).get_or_insert_with(|| Instant::now() + timing::EXIT_WAIT_TIMEOUT);
        ensure!(
            Instant::now() < deadline,
            "the daemon held the request back for its exit and no daemon took it within {} minutes",
            timing::EXIT_WAIT_TIMEOUT.as_secs() / 60
        );
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
        && SUPERVISION
            .get()
            .is_some_and(|supervision| supervision.managed(paths))
}

async fn connect(paths: &Paths) -> Result<UnixStream> {
    UnixStream::connect(&paths.socket)
        .await
        .map_err(|error| match unresponsive(paths) {
            Some(unresponsive) => anyhow::Error::new(error).context(unresponsive),
            None => anyhow::Error::new(error).context(format!(
                "connect to {}; run `shoal daemon install` or `shoal daemon start`",
                paths.socket.display()
            )),
        })
}

async fn reply(stream: &mut (impl AsyncBufRead + Unpin), request: &Request) -> Result<Body> {
    let reply = protocol::read_response(stream).await?;
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
pub async fn call(daemon: &impl Daemon, method: impl Into<Method>) -> Result<Body> {
    let method = method.into();
    let duration = if method.controls_daemon() {
        timing::ADMIN_REQUEST_TIMEOUT
    } else {
        timing::REQUEST_TIMEOUT
    };
    let _span = crate::profile::span("call", || method.name());
    let (_stream, body) = timeout(duration, open(daemon, method))
        .await
        .map_err(|_| {
            let error = anyhow::anyhow!("daemon request timed out");
            match unresponsive(daemon.paths()) {
                Some(unresponsive) => error.context(unresponsive),
                None => error,
            }
        })??;
    body.into_result()
}

/// Send a request and extract its expected response payload.
pub async fn request<T>(daemon: &impl Daemon, method: impl Into<Method>) -> Result<T>
where
    T: TryFrom<Body, Error = anyhow::Error>,
{
    T::try_from(call(daemon, method).await?)
}

/// The items of every page the daemon sends for `method` before `ok`.
pub async fn pages<T>(daemon: &impl Daemon, method: impl Into<Method>) -> Result<Vec<T>>
where
    Vec<T>: TryFrom<Body, Error = anyhow::Error>,
{
    let method = method.into();
    let _span = crate::profile::span("call", || method.name());
    let (mut reader, body) = open_buffered(daemon, method).await?;
    let mut items = Vec::<T>::try_from(body)?;
    loop {
        let response = protocol::read_response(&mut reader).await?;
        match response.body {
            Body::Ok => return Ok(items),
            body => items.extend(Vec::<T>::try_from(body)?),
        }
    }
}

/// The `what` the daemon sends for `method` as pieces of its JSON, joined.
pub async fn joined<T: DeserializeOwned>(
    daemon: &impl Daemon,
    method: impl Into<Method>,
    what: &str,
) -> Result<T> {
    let pieces = pages::<ViewPiece>(daemon, method).await?;
    let json: String = pieces.into_iter().map(|piece| piece.0).collect();
    serde_json::from_str(&json).with_context(|| format!("the daemon sent an invalid {what}"))
}

/// `None` when no daemon is running; one that runs without answering is an
/// [`Unresponsive`] error.
pub async fn status(daemon: &impl Daemon) -> Result<Option<DaemonStatus>> {
    match request(daemon, DaemonMethod::Status).await {
        Ok(status) => Ok(Some(status)),
        Err(error) if not_running(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Ask a running daemon to reread the global config; `false` when none is
/// running, since a starting daemon reads it anyway.
pub async fn reload_config(daemon: &impl Daemon) -> Result<bool> {
    match call(daemon, DaemonMethod::ReloadConfig).await {
        Ok(_) => Ok(true),
        Err(error) if not_running(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Ask the daemon to exit as `request` says, whatever its release. A daemon
/// that waits for work its exit would cut off sends that work, which
/// `waiting` hears each time it changes, until it agrees to exit. A refusal
/// fails.
pub async fn exit(
    daemon: &impl Daemon,
    request: ExitRequest,
    mut waiting: impl FnMut(&[Work]),
) -> Result<ExitAnswer> {
    let when = request.when;
    let request = request_to(daemon, request);
    let asked = async {
        let stream = UnixStream::connect(&daemon.paths().socket).await.ok()?;
        let mut stream = BufReader::new(stream);
        protocol::write(stream.get_mut(), &request).await.ok()?;
        let response = protocol::read_buffered::<Response>(&mut stream)
            .await
            .ok()?;
        Some((response, stream))
    };
    // The answer reads the same in every protocol version.
    let Ok(Some((response, mut stream))) = timeout(timing::ADMIN_REQUEST_TIMEOUT, asked).await
    else {
        return Ok(ExitAnswer::Unanswered);
    };
    let mut waited_for = Vec::<Work>::new();
    let mut body = response.body;
    loop {
        match body {
            Body::Ok => return Ok(ExitAnswer::Exiting(Exiting { stream, waited_for })),
            Body::ExitWaits(work) if when == ExitWhen::IfIdle && Work::refuses_idle_exit(&work) => {
                return Ok(ExitAnswer::Refused(work));
            }
            Body::ExitWaits(work) => {
                waiting(&work);
                Work::list_new(&mut waited_for, work);
            }
            Body::Error {
                code: ErrorCode::InvalidRequest | ErrorCode::ProtocolMismatch,
                ..
            } => return Ok(ExitAnswer::Unanswered),
            body => return body.into_result().map(|_| ExitAnswer::Unanswered),
        }
        // The work waited for may take long, and the daemon may go meanwhile,
        // as for an update or another exit.
        body = match protocol::read_buffered::<Response>(&mut stream).await {
            Ok(response) => response.body,
            Err(_) => return Ok(ExitAnswer::Gone(waited_for)),
        };
    }
}

/// How the daemon answered a request to exit.
pub enum ExitAnswer {
    /// It agreed to exit.
    Exiting(Exiting),
    /// It runs the work, which its exit would cut off, and stays.
    Refused(Vec<Work>),
    /// It went while it waited for the work, which a daemon that follows
    /// may still run.
    Gone(Vec<Work>),
    /// No daemon answered the request, as when none runs, it is still
    /// starting, or its release predates the request.
    Unanswered,
}

/// A daemon that agreed to exit, on the connection that hears what it left.
pub struct Exiting {
    stream: BufReader<UnixStream>,
    /// The work it waited for before it agreed, each once.
    pub waited_for: Vec<Work>,
}

impl Exiting {
    /// Whether the daemon left stopped work for `shoal restore`, once its
    /// work has ended; `None` when it does not say, as a daemon of an
    /// earlier release does not.
    pub async fn stopped_work(mut self) -> Option<bool> {
        let limit = timing::DAEMON_WAIT_TIMEOUT + timing::WORKSPACE_STOP_TIMEOUT;
        let read = protocol::read_buffered::<Response>(&mut self.stream);
        match timeout(limit, read).await {
            Ok(Ok(Response {
                body: Body::Exited(exited),
                ..
            })) => Some(exited.stopped_work),
            _ => None,
        }
    }
}

/// No daemon listens on the socket or owns the state directory.
fn not_running(error: &anyhow::Error) -> bool {
    is_unreachable(error) && !error.is::<Unresponsive>()
}

/// No daemon listens on the socket, though one may own the state directory.
pub fn is_unreachable(error: &anyhow::Error) -> bool {
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

/// The daemon closed the connection, as it does across a restart.
pub fn stream_closed(error: &anyhow::Error) -> bool {
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
pub async fn reconnect<M: Into<Method>>(
    daemon: &impl Daemon,
    method: impl Fn() -> M,
) -> Result<(BufReader<UnixStream>, Body)> {
    timeout(timing::DAEMON_WAIT_TIMEOUT, async {
        loop {
            match open_buffered(daemon, method()).await {
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
pub async fn await_reply<M: Into<Method>>(
    daemon: &impl Daemon,
    method: impl Fn() -> M,
    restarted: bool,
) -> Result<Body> {
    let mut outage = restarted.then(Outage::restart);
    loop {
        let reply = open(daemon, method()).await;
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

/// The result of `ask`, requests that are safe to repeat, from a daemon
/// that restarted: an unreachable socket, a closed connection or a scope
/// refusal is tried again within the restart budget, since a restarted
/// daemon knows an execution's scope only once its wrapper reattaches.
pub async fn after_restart<T, F>(ask: impl Fn() -> F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let deadline = Instant::now() + timing::DAEMON_RESTART_TIMEOUT;
    loop {
        match ask().await {
            Err(error)
                if Instant::now() < deadline
                    && (is_unreachable(&error)
                        || stream_closed(&error)
                        || scope_denied(&error)) =>
            {
                sleep(timing::DAEMON_POLL_INTERVAL).await;
            }
            answer => return answer,
        }
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

pub fn scope_denied(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<protocol::RemoteError>()
        .is_some_and(|remote| remote.code == protocol::ErrorCode::ScopeDenied)
}

/// Wait for the daemon to be running (or stopped).
pub async fn wait(daemon: &impl Daemon, running: bool) -> Result<()> {
    // A stopping daemon first stops its connected executions.
    let limit = if running {
        timing::DAEMON_WAIT_TIMEOUT
    } else {
        timing::DAEMON_WAIT_TIMEOUT + timing::WORKSPACE_STOP_TIMEOUT
    };
    let deadline = Instant::now() + limit;
    loop {
        let last_error = match status(daemon).await {
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
mod tests;
