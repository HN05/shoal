//! The client side of the daemon protocol, which the CLI and apps share:
//! finding the daemon, sending one request per connection with the caller's
//! scope token, reading its reply, and typed helpers for the queries
//! commands share.
use std::{io, sync::OnceLock, time::Duration};

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
    client::{paths::Paths, processes::Identity},
    config::repo::ConfigLayers,
    model::{Chat, Inspection, Repository, Workspace},
    protocol::{
        self, Body, ChatMethod, DaemonMethod, DaemonStatus, ErrorCode, Exit, ItemMethod, Method,
        RepositoryMethod, Request, Response, WorkspaceMethod, timing,
    },
};

/// A daemon to send requests to, and the scope they carry.
pub trait Daemon: Sync {
    /// Where the daemon listens and keeps its state.
    fn paths(&self) -> &Paths;
    /// The scope token requests carry: with one, the caller is the agent of
    /// the workspace it belongs to; without one, the user.
    fn scope(&self) -> Option<String>;
}

/// The CLI's daemon: requests carry the scope token this process inherited.
impl Daemon for Paths {
    fn paths(&self) -> &Paths {
        self
    }

    fn scope(&self) -> Option<String> {
        crate::client::env::scope_token()
    }
}

/// The daemon an app connects to, with the scope it chose.
#[derive(Debug, Clone)]
pub struct Connector {
    pub paths: Paths,
    pub scope: Option<String>,
}

impl Connector {
    /// The daemon `shoal` uses: in the state directory `SHOAL_STATE_DIR`
    /// names, else the default one, with no scope token.
    pub fn new() -> Result<Self> {
        let state = std::env::var_os(crate::client::env::STATE_DIR).map(Into::into);
        Ok(Self {
            paths: Paths::new(state)?,
            scope: None,
        })
    }

    /// Requests as the agent of the workspace `token` belongs to.
    pub fn scope(self, token: impl Into<String>) -> Self {
        Self {
            scope: Some(token.into()),
            ..self
        }
    }
}

impl Daemon for Connector {
    fn paths(&self) -> &Paths {
        &self.paths
    }

    fn scope(&self) -> Option<String> {
        self.scope.clone()
    }
}

/// What the `shoal` command adds to the connections it makes: telling a
/// daemon that stopped answering from one that is not running, and waiting
/// for a managed daemon to restart with an installed update. Apps connect
/// without it.
pub(crate) trait Supervision: Send + Sync {
    /// The daemon that owns the state directory of `paths`, when one does.
    fn owner(&self, paths: &Paths) -> Option<Identity>;
    /// Whether a service manager restarts the daemon of `paths`.
    fn managed(&self, paths: &Paths) -> bool;
    /// Tell the user a request waits for the daemon's restart.
    fn waiting_for_update(&self);
}

static SUPERVISION: OnceLock<&'static dyn Supervision> = OnceLock::new();

/// Supervise this process's connections with `supervision`.
#[cfg(feature = "cli")]
pub(crate) fn supervise(supervision: &'static dyn Supervision) {
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
    let mut update = UpdateWait::default();
    loop {
        let mut stream = connect(daemon.paths()).await?;
        protocol::write(&mut stream, &request).await?;
        let mut stream = BufReader::new(stream);
        match reply(&mut stream, &request).await {
            Ok(body) => return Ok((stream, body)),
            Err(error) => update.retry(daemon.paths(), &request, error).await?,
        }
    }
}

/// [`open_buffered`] with the stream split, so a task can own the receive
/// half. `limit` bounds each reply, not a wait for an update.
pub async fn open_split(
    daemon: &impl Daemon,
    method: impl Into<Method>,
    limit: Duration,
) -> Result<(BufReader<OwnedReadHalf>, OwnedWriteHalf, Body)> {
    let request = request_to(daemon, method);
    let mut update = UpdateWait::default();
    loop {
        let (reader, mut writer) = connect(daemon.paths()).await?.into_split();
        protocol::write(&mut writer, &request).await?;
        let mut reader = BufReader::new(reader);
        let replied = timeout(limit, reply(&mut reader, &request))
            .await
            .context("daemon did not reply in time")?;
        match replied {
            Ok(body) => return Ok((reader, writer, body)),
            Err(error) => update.retry(daemon.paths(), &request, error).await?,
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

/// The forge's JSON response to `request` in `workspace`'s repository,
/// reassembled from the pieces the daemon sends it in.
pub async fn forge_api(
    daemon: &impl Daemon,
    context: crate::protocol::ItemContext,
    request: crate::forge::raw::RawRequest,
    repository: Option<String>,
) -> Result<serde_json::Value> {
    let method = ItemMethod::ForgeApi {
        context,
        request,
        repository,
    };
    let pieces = pages::<String>(daemon, method).await?;
    serde_json::from_str(&pieces.concat()).context("the daemon sent invalid JSON")
}

/// What a review request returns, a review or a listing of them,
/// reassembled from the pieces of JSON the daemon sends it in.
pub async fn reviews<T: serde::de::DeserializeOwned>(
    daemon: &impl Daemon,
    method: crate::protocol::ReviewMethod,
) -> Result<T> {
    let pieces = pages::<crate::protocol::ReviewPiece>(daemon, method).await?;
    let json: String = pieces.into_iter().map(|piece| piece.0).collect();
    serde_json::from_str(&json).context("the daemon sent an invalid review")
}

/// The file at `url` on the forge of `context`'s repository, or of the one
/// `repository` names, reassembled from the pieces the daemon sends it in.
pub async fn forge_download(
    daemon: &impl Daemon,
    context: crate::protocol::ItemContext,
    url: String,
    repository: Option<String>,
) -> Result<crate::forge::download::Download> {
    let method = ItemMethod::ForgeDownload {
        context,
        url,
        repository,
    };
    let pieces = pages(daemon, method).await?;
    crate::protocol::DownloadPiece::join(pieces)
}

/// The settings that apply to `target` after every layer. The global config
/// is read now, so launch defaults follow it without a daemon restart.
pub async fn settings(
    daemon: &impl Daemon,
    target: crate::protocol::ConfigTarget,
) -> Result<crate::config::Effective> {
    Ok(configuration(daemon, target).await?.1)
}

/// The global config read at launch and the settings it yields for `target`.
pub async fn configuration(
    daemon: &impl Daemon,
    target: crate::protocol::ConfigTarget,
) -> Result<(crate::config::Config, crate::config::Effective)> {
    let layers =
        request::<Box<ConfigLayers>>(daemon, WorkspaceMethod::LayeredConfig { target }).await?;
    let config = crate::config::Config::load_with_templates(daemon.paths())?;
    let settings = config.resolve(&layers)?;
    Ok((config, settings))
}

pub async fn inspect(daemon: &impl Daemon, workspace: String) -> Result<Inspection> {
    request(daemon, WorkspaceMethod::InspectWorkspace { workspace }).await
}

/// Workspaces visible to this caller; the daemon filters scoped requests.
pub async fn workspaces(daemon: &impl Daemon) -> Result<Vec<Workspace>> {
    request(daemon, WorkspaceMethod::ListWorkspaces).await
}

/// Record the chat `name` whose agent replaces this process, running at
/// `origin` in a terminal host when it runs in one, or when `hands_over`
/// runs in a session this process starts and [`move_chat`] names.
pub async fn start_chat(
    daemon: &impl Daemon,
    repository: &Repository,
    name: &str,
    agent: &str,
    origin: Option<crate::client::sessions::Origin>,
    hands_over: bool,
) -> Result<Chat> {
    request(
        daemon,
        ChatMethod::StartChat {
            repository: repository.id.clone().into(),
            name: name.to_owned(),
            agent: agent.to_owned(),
            pid: std::process::id(),
            origin,
            hands_over,
        },
    )
    .await
}

/// Refuse a name a new chat could not take now.
pub async fn check_chat_name(daemon: &impl Daemon, name: &str) -> Result<()> {
    request::<()>(
        daemon,
        ChatMethod::CheckChatName {
            name: name.to_owned(),
        },
    )
    .await
}

/// `name`, or its lowest `-2`, `-3`, ... variant, that a new chat could take now.
pub async fn free_chat_name(daemon: &impl Daemon, name: String) -> Result<String> {
    request(daemon, ChatMethod::FreeChatName { name }).await
}

/// Hand `chat` to process `pid`, whose agent now runs it.
pub async fn move_chat(daemon: &impl Daemon, chat: &Chat, pid: u32) -> Result<Chat> {
    request(
        daemon,
        ChatMethod::MoveChat {
            chat: chat.id.clone(),
            pid,
        },
    )
    .await
}

/// Running chats; listing them ends those whose agent exited.
pub async fn chats(daemon: &impl Daemon) -> Result<Vec<Chat>> {
    let mut chats = recorded_chats(daemon).await?;
    chats.retain(|chat| chat.restore.is_none());
    Ok(chats)
}

/// Running chats and those Shoal saved to restore.
pub async fn recorded_chats(daemon: &impl Daemon) -> Result<Vec<Chat>> {
    request(daemon, ChatMethod::ListChats).await
}

/// [`start_chat`], or for a start that `restores` a saved chat, by ID,
/// [`restore_chat`], which keeps the chat's own name and agent.
pub async fn record_chat(
    daemon: &impl Daemon,
    repository: &Repository,
    name: &str,
    agent: &str,
    origin: Option<crate::client::sessions::Origin>,
    hands_over: bool,
    restores: Option<&str>,
) -> Result<Chat> {
    match restores {
        Some(chat) => restore_chat(daemon, chat, origin, hands_over).await,
        None => start_chat(daemon, repository, name, agent, origin, hands_over).await,
    }
}

/// Run the saved chat `chat`, by ID, again in this process, as
/// [`start_chat`] records a new chat.
pub async fn restore_chat(
    daemon: &impl Daemon,
    chat: &str,
    origin: Option<crate::client::sessions::Origin>,
    hands_over: bool,
) -> Result<Chat> {
    request(
        daemon,
        ChatMethod::RestoreChat {
            chat: chat.to_owned(),
            pid: std::process::id(),
            origin,
            hands_over,
        },
    )
    .await
}

/// End the saved chat `chat`, by ID or name, without restoring it.
pub async fn discard_chat(daemon: &impl Daemon, chat: &str) -> Result<Chat> {
    request(
        daemon,
        ChatMethod::DiscardChat {
            chat: chat.to_owned(),
        },
    )
    .await
}

pub async fn repositories(daemon: &impl Daemon) -> Result<Vec<Repository>> {
    request(daemon, RepositoryMethod::ListRepositories).await
}

/// Close `place`, a place Shoal created for `workspace`, while the daemon
/// still records it as that workspace's.
pub async fn close_owned_place(
    daemon: &impl Daemon,
    workspace: &str,
    place: &crate::client::sessions::Origin,
) -> Result<crate::client::sessions::PlaceClose> {
    request(
        daemon,
        WorkspaceMethod::CloseOwnedPlace {
            workspace: workspace.to_owned(),
            place: place.clone(),
        },
    )
    .await
}

/// Whose `place`, the place of a terminal host, is for `workspace`, as the
/// daemon knows it.
pub async fn place_ownership(
    daemon: &impl Daemon,
    workspace: &str,
    place: &crate::client::sessions::Origin,
) -> Result<crate::client::sessions::PlaceOwnership> {
    request(
        daemon,
        WorkspaceMethod::PlaceOwnership {
            workspace: workspace.to_owned(),
            place: place.clone(),
        },
    )
    .await
}

/// Record `origin`, a place Shoal created for `workspace`, so the daemon
/// closes it once the workspace is removed.
pub async fn record_owned_place(
    daemon: &impl Daemon,
    workspace: &str,
    origin: crate::client::sessions::Origin,
) -> Result<()> {
    call(
        daemon,
        WorkspaceMethod::RecordOwnedPlace {
            workspace: workspace.to_owned(),
            origin,
        },
    )
    .await?;
    Ok(())
}

/// Keep where an agent of `workspace` keeps its configuration for the
/// repository's removal.
pub async fn keep_agent_environment(
    daemon: &impl Daemon,
    workspace: &str,
    environment: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    call(
        daemon,
        WorkspaceMethod::KeepAgentEnvironment {
            workspace: workspace.to_owned(),
            environment,
        },
    )
    .await?;
    Ok(())
}

/// [`keep_agent_environment`] for a chat agent of `repository`.
pub async fn keep_repository_agent_environment(
    daemon: &impl Daemon,
    repository: &str,
    environment: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    call(
        daemon,
        RepositoryMethod::KeepRepositoryAgentEnvironment {
            repository: repository.to_owned(),
            environment,
        },
    )
    .await?;
    Ok(())
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

/// Ask the daemon to exit with `exit`, whatever its release; `false` when no
/// daemon answers the request, as when none runs, it is still starting, or
/// its release predates the request. A refusal fails.
pub async fn exit(daemon: &impl Daemon, exit: Exit) -> Result<bool> {
    let request = request_to(daemon, exit);
    let asked = async {
        let mut stream = UnixStream::connect(&daemon.paths().socket).await.ok()?;
        protocol::write(&mut stream, &request).await.ok()?;
        protocol::read::<Response>(&mut stream).await.ok()
    };
    // The answer reads the same in every protocol version.
    let Ok(Some(response)) = timeout(timing::ADMIN_REQUEST_TIMEOUT, asked).await else {
        return Ok(false);
    };
    match response.body {
        Body::Ok => Ok(true),
        Body::Error {
            code: ErrorCode::InvalidRequest | ErrorCode::ProtocolMismatch,
            ..
        } => Ok(false),
        body => body.into_result().map(|_| false),
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

pub(crate) fn scope_denied(error: &anyhow::Error) -> bool {
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
