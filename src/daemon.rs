//! The per-user daemon: owns shared state, serves protocol requests over a
//! Unix socket, and runs background cleanup.
pub mod access;
pub mod agent_messages;
pub(crate) mod agent_queue;
pub mod allocation;
mod answer;
pub mod approvals;
mod auto_update;
pub(crate) mod caches;
pub mod chat_messages;
mod chats;
pub mod cleanup;
mod connections;
pub(crate) mod cpu_usage;
mod disk;
pub mod environment;
pub mod events;
mod execution_connection;
mod execution_recovery;
pub(crate) mod forge_hosts;
pub(crate) mod handoff;
mod keep_awake;
pub mod links;
pub(crate) mod log;
pub mod notifications;
mod overload;
#[cfg(test)]
mod overload_tests;
mod ownership;
pub mod ports;
pub mod questions;
pub(crate) mod queue;
pub mod recovery;
mod requests;
pub mod resources;
mod restart_recovery;
pub mod scope;
pub mod secrets;
pub(crate) mod seed;
mod startup_audit;
pub mod store;
mod untracked;
mod warnings;
pub mod workspace;
mod workspace_usage;

use std::{
    panic::AssertUnwindSafe,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use futures_util::FutureExt;
use tokio::{
    net::UnixStream,
    signal::unix::{SignalKind, signal},
    sync::watch,
    task::JoinSet,
    time::timeout,
};

use answer::OperationGuard;
pub(crate) use log::log;
use ownership::Ownership;
use scope::Caller;
use workspace::Manager;

use crate::{
    paths::Paths,
    protocol::{self, Body, ErrorCode, Method, Operation, Reattachment, Request, Response, timing},
};

const SIMULATOR_SWEEP_INTERVAL: Duration = Duration::from_secs(15);
const ITEM_ACTIVITY_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// Keeps a background loop that panics at once from spinning.
const RESTART_DELAY: Duration = Duration::from_secs(10);

/// Settings fixed for the daemon's lifetime and shared by every connection.
#[derive(Clone)]
struct Server {
    manager: Arc<Manager>,
    started: Instant,
    /// Running under the OS service manager, which owns shutdown.
    managed: bool,
    shutdown: watch::Sender<bool>,
    capacity: Arc<connections::Capacity>,
}

pub async fn run(paths: Paths, managed: bool, handoff: Option<handoff::Handoff>) -> Result<()> {
    let update = auto_update::Update::new(managed);
    paths.prepare()?;
    let descriptors = connections::raise_descriptor_limit();
    let mut ownership = Ownership::acquire(&paths, managed, handoff)?;
    let manager = Manager::open(paths.clone()).await?;
    manager.store.quarantine_interrupted_operations().await?;
    // Finishing a move restores the states quarantine failed.
    if let Err(error) = manager.finish_repository_moves().await {
        log!("{error:#}");
    }
    manager.learn_recorded_forge_hosts().await?;
    manager.audit_worktrees().await?;
    manager.cleanup_orphan_sessions().await;
    let listener = ownership.listen()?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let mut update_tick = tokio::time::interval(Duration::from_secs(1));
    let server = Server {
        manager: manager.clone(),
        started: Instant::now(),
        managed,
        shutdown,
        capacity: Arc::new(connections::Capacity::for_descriptors(descriptors)),
    };
    log!("shoal daemon listening on {}", paths.socket.display());
    let mut background = JoinSet::new();
    spawn_supervised(&mut background, &manager, "auto cleanup", cleanup::run);
    spawn_supervised(
        &mut background,
        &manager,
        "environment check",
        environment::watch_environment,
    );
    spawn_supervised(
        &mut background,
        &manager,
        "restart reconciliation",
        restart_recovery::run,
    );
    spawn_supervised(&mut background, &manager, "overload monitor", overload::run);
    spawn_supervised(&mut background, &manager, "disk monitor", disk::run);
    spawn_supervised(&mut background, &manager, "CPU usage", cpu_usage::run);
    spawn_supervised(
        &mut background,
        &manager,
        "workspace usage",
        workspace_usage::run,
    );
    spawn_supervised(&mut background, &manager, "keep awake", keep_awake::run);
    spawn_supervised(&mut background, &manager, "agent queue", agent_queue::run);
    spawn_supervised(
        &mut background,
        &manager,
        "turn retries",
        workspace::turn_retry::run,
    );
    spawn_supervised(&mut background, &manager, "chats", chats::run);
    spawn_supervised(
        &mut background,
        &manager,
        "simulator cleanup",
        expire_simulators,
    );
    spawn_supervised(&mut background, &manager, "seed builds", seed::run);
    spawn_supervised(
        &mut background,
        &manager,
        "item activity",
        poll_item_activity,
    );
    let mut clients = JoinSet::new();
    let mut quiescence = None;
    let result = loop {
        tokio::select! {
            _ = terminate.recv() => break Ok(()),
            _ = interrupt.recv() => break Ok(()),
            _ = shutdown_rx.changed() => break Ok(()),
            _ = update_tick.tick(), if update.is_some() => {
                if update.as_ref().is_some_and(auto_update::Update::pending)
                    && let Some(guard) = auto_update::quiesce(&manager)
                {
                    quiescence = Some(guard);
                    break Ok(());
                }
            },
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            accepted = connections::accept(&listener) => {
                let stream = match accepted {
                    Ok(stream) => stream,
                    Err(error) => {
                        log!("stopped accepting connections: {error}");
                        break Err(error.into());
                    }
                };
                let server = server.clone();
                let operation = server.manager.gates.owned_operation().await;
                clients.spawn(async move {
                    if let Err(error) = serve(stream, server, operation).await {
                        log!("client connection: {error:#}");
                    }
                });
            }
        }
    };
    background.shutdown().await;
    manager.stop_for_shutdown().await;
    clients.shutdown().await;
    manager.store.shutdown().await;
    if quiescence.is_some() {
        log!("daemon executable was updated; restarting");
        return ownership.hand_off(&update.expect("update monitor enabled").path, &listener);
    }
    result
}

/// Register a manager-owned loop with panic supervision.
fn spawn_supervised<F, Fut>(
    background: &mut JoinSet<()>,
    manager: &Arc<Manager>,
    name: &'static str,
    task: F,
) where
    F: Fn(Arc<Manager>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let manager = manager.clone();
    background.spawn(supervise(name, move || task(manager.clone())));
}

/// Run a background loop again after it panics, so one failure cannot stop it
/// for the rest of the daemon's life.
async fn supervise<F, Fut>(name: &'static str, mut task: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    while AssertUnwindSafe(task()).catch_unwind().await.is_err() {
        log!(
            "{name} panicked; restarting in {}s",
            RESTART_DELAY.as_secs()
        );
        tokio::time::sleep(RESTART_DELAY).await;
    }
}

async fn expire_simulators(manager: Arc<Manager>) {
    loop {
        tokio::time::sleep(SIMULATOR_SWEEP_INTERVAL).await;
        let _operation = manager.gates.operation().await;
        if let Err(error) = manager.expire_simulators().await {
            log!("simulator cleanup: {error:#}");
        }
    }
}

async fn poll_item_activity(manager: Arc<Manager>) {
    let mut interval = tokio::time::interval(ITEM_ACTIVITY_SWEEP_INTERVAL);
    loop {
        interval.tick().await;
        let _operation = manager.gates.operation().await;
        if let Err(error) = manager.poll_linked_item_activity().await {
            log!("item activity: {error:#}");
        }
        if let Err(error) = manager.poll_dependency_issues().await {
            log!("agent queue dependencies: {error:#}");
        }
    }
}

/// Answer one connection's request: read it, admit it, resume an execution
/// or accept it, then answer it.
async fn serve(
    mut stream: UnixStream,
    server: Server,
    operation_guard: OperationGuard,
) -> Result<()> {
    let Some(request) = read_request(&mut stream).await? else {
        return Ok(());
    };
    let _span = request_span(&request);
    let Some(_slot) = admit(&mut stream, &server, &request).await? else {
        return Ok(());
    };
    let name = request.method.name();
    let mut operation = match request.method {
        // Wrappers from any release reattach, and their launch scope does not
        // survive the restart; the record and token they present prove ownership.
        Method::Reattach(Reattachment::Reattach(reattach)) => {
            return execution_connection::reattach(
                stream,
                request.id,
                server.manager,
                reattach,
                operation_guard,
            )
            .await;
        }
        Method::Operation(operation) => operation,
    };
    let scope = request.scope.as_deref();
    let accepted = accept(
        &mut stream,
        &server,
        request.id,
        request.protocol,
        scope,
        &mut operation,
    );
    let Some(caller) = accepted.await? else {
        return Ok(());
    };
    let answer = requests::operation(&server, operation, caller.as_ref());
    let answer = connections::running::logged_when_slow(&name, answer).await;
    answer
        .send(stream, request.id, &server.manager, operation_guard)
        .await
}

/// The client's request, or `None` once an unreadable one was refused.
async fn read_request(stream: &mut UnixStream) -> Result<Option<Request>> {
    match timeout(timing::REQUEST_READ_TIMEOUT, protocol::read(stream)).await? {
        Ok(request) => Ok(Some(request)),
        Err(error) => {
            let response = Response::new(0, Body::error(ErrorCode::InvalidRequest, error));
            protocol::write(stream, &response).await?;
            Ok(None)
        }
    }
}

/// A profile span over the request, except one that keeps its connection.
fn request_span(request: &Request) -> Option<crate::profile::Span> {
    if request.method.holds_connection() {
        None
    } else {
        crate::profile::span("request", || request.method.name())
    }
}

/// The connection slot the request keeps, or `None` once it was refused.
async fn admit(
    stream: &mut UnixStream,
    server: &Server,
    request: &Request,
) -> Result<Option<connections::Slot>> {
    match server.capacity.admit(&request.method) {
        Ok(slot) => Ok(Some(slot)),
        // Their clients take a reply as final but wait for a free slot this way.
        Err(_) if request.method.retries_when_closed() => Ok(None),
        Err(error) => {
            let body = Body::error(ErrorCode::OperationFailed, format!("{error:#}"));
            protocol::write(stream, &Response::new(request.id, body)).await?;
            Ok(None)
        }
    }
}

/// The caller of a request this daemon's protocol reads and its scope
/// allows, narrowing the request to that scope, or `None` once it was refused.
async fn accept(
    stream: &mut UnixStream,
    server: &Server,
    request_id: u64,
    protocol: u32,
    scope: Option<&str>,
    operation: &mut Operation,
) -> Result<Option<Option<Caller>>> {
    if protocol != protocol::VERSION {
        let body = Body::error(
            ErrorCode::ProtocolMismatch,
            "restart the daemon with the installed version",
        );
        protocol::write(stream, &Response::new(request_id, body)).await?;
        return Ok(None);
    }
    match scope::authorize(&server.manager, scope, operation).await {
        Ok(caller) => Ok(Some(caller)),
        Err(error) => {
            let body = Body::error(ErrorCode::ScopeDenied, format!("{error:#}"));
            protocol::write(stream, &Response::new(request_id, body)).await?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests;
