//! The per-user daemon: owns shared state, serves protocol requests over a
//! Unix socket, and runs background cleanup.
#[cfg(target_os = "linux")]
pub(crate) use shoal_agents::oom_scores;
pub(crate) use shoal_agents::{capacity, overload, queue as agent_queue, queued_starts};
pub(crate) use shoal_agents::{
    control as agents, execution_connection, executions, idle_wakes, lost_reports, messages,
    places, restart_recovery, restarts, shutdown_stop, stopped_work, turn_retry, turn_state,
    wake_timers, warnings,
};
pub(crate) use shoal_daemon_base::{
    config_state, events, file_access, gates, item_numbers, log, notifications, retention, signals,
};
pub(crate) use shoal_leases::{allocation, containers, resources};
mod absorb;
mod agent_service;
mod alerts;
mod answer;
pub mod approvals;
mod apps;
mod auto_update;
pub(crate) mod blockers;
mod carried_commits;
mod carried_sessions;
pub(crate) mod changes;
pub(crate) mod chat_marks;
pub(crate) mod chats;
pub mod cleanup;
pub(crate) mod completion_cleanup;
mod connections;
mod contribution_holds;
pub(crate) mod conversions;
mod coordinated;
mod daemon_exit;
pub(crate) mod demos;
pub(crate) mod disk;
mod elsewhere;
pub mod environment;
mod exit_wait;
pub(crate) mod fake_workers;
pub(crate) mod forge_hosts;
pub(crate) mod gate;
mod git_logins;
pub(crate) mod holds;
mod hook_settings;
pub(crate) mod hub;
mod hub_moves;
mod idle_sleep;
mod imported_sessions;
mod launches;
mod leases;
pub mod leftovers;
mod lifecycle;
pub mod links;
#[cfg(target_os = "linux")]
mod logind;
mod logs;
pub mod manager;
mod moves;
pub(crate) mod operations;
mod ownership;
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod per_daemon;
mod permissions;
mod pr_gate;
pub(crate) mod progress;
mod quarantine;
pub(crate) mod ready_marks;
pub mod recovery;
mod relayed;
mod releases;

mod repositories;
mod repository_removals;
mod requests;
mod restore;
mod retention_periods;
pub(crate) mod reviews;
pub(crate) mod runner;
pub mod scope;
mod shared_repositories;
mod shutdown_lock;
mod sleep_lock;
pub(crate) mod space_processes;
mod space_removal;
pub(crate) mod spaces;
mod startup;
mod startup_audit;
pub(crate) use shoal_store as store;
mod subscriptions;
pub(crate) mod target_checks;
#[cfg(any(test, feature = "test-support"))]
// Other crates' tests read most of it.
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
pub(crate) mod test_daemon;
#[cfg(any(test, feature = "test-support"))]
// Other crates' tests read only some of the daemon's own fixtures.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod test_support;
pub mod threads;
mod tmp_dirs;
mod tree;

mod turn_ends;
mod untracked;
mod viewer_terminals;
mod work_memory;
pub mod workspace;
pub mod workspace_files;
mod workspace_usage;

use shoal_protocol::scope_tokens::ScopeToken;
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
    sync::mpsc,
    task::JoinSet,
    time::timeout,
};

use answer::OperationGuard;
use daemon_exit::Ending;
use events::{ShoalEventDetails, ShoalEventKind};
use manager::Manager;
use notifications::AlertKind;
use ownership::Ownership;
use scope::Caller;

use crate::{
    paths::Paths,
    protocol::{self, Body, ErrorCode, Method, Operation, Reattachment, Request, Response, timing},
};
use shoal_protocol::hub::link::Opening;

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
    /// Ends the daemon, with what becomes of running work and the client
    /// that asked.
    exit: mpsc::Sender<requests::daemon::RequestedExit>,
    capacity: Arc<connections::Capacity>,
}

pub async fn run(
    paths: Paths,
    managed: bool,
    handoff: Option<crate::daemon_handoff::Handoff>,
) -> Result<()> {
    // Read before an upgrade can replace it, as the update below is, since
    // wrappers continue only from the binary the daemon runs.
    crate::installed::remember_started();
    let update = auto_update::Update::new(managed);
    paths.prepare()?;
    let descriptors = connections::raise_descriptor_limit();
    let mut ownership = Ownership::acquire(&paths, managed, handoff)?;
    let startup = startup::Startup::begin(ownership.listen()?);
    log!("shoal daemon listening on {}", paths.socket.display());
    // Before it starts any place or worker, which keep its score.
    #[cfg(target_os = "linux")]
    oom_scores::lower_own_score();
    let manager = start(&startup, &paths).await?;
    if managed {
        crate::service::apart::start_apart_from_service().await;
        manager.refresh_service_definition().await;
    }
    manager.record_start().await;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let (exit, mut exit_requested) = mpsc::channel(1);
    let mut update_tick = tokio::time::interval(Duration::from_secs(1));
    let server = Server {
        manager: manager.clone(),
        started: Instant::now(),
        managed,
        exit,
        capacity: Arc::new(connections::Capacity::for_descriptors(descriptors)),
    };
    let resumes_until =
        tokio::time::Instant::now() + shoal_repositories::contributions::holds::RESUME_WINDOW;
    manager
        .repositories
        .contribution_holds()
        .await_resumes(resumes_until);
    let mut background = JoinSet::new();
    spawn_supervised(&mut background, &manager, "auto cleanup", cleanup::run);
    spawn_supervised(&mut background, &manager, "checkout sync", |manager| {
        shoal_repositories::sync::run(manager.repositories.clone(), manager)
    });
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
        |manager| restart_recovery::run(manager.agents.clone(), manager),
    );
    spawn_supervised(&mut background, &manager, "overload monitor", |manager| {
        overload::run(manager.agents.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "disk monitor", disk::run);
    #[cfg(target_os = "linux")]
    spawn_supervised(&mut background, &manager, "OOM scores", |manager| {
        oom_scores::run(manager.agents.clone(), manager)
    });
    #[cfg(target_os = "linux")]
    spawn_supervised(&mut background, &manager, "work memory", work_memory::run);
    spawn_supervised(
        &mut background,
        &manager,
        "workspace usage",
        workspace_usage::run,
    );
    spawn_supervised(&mut background, &manager, "keep awake", |manager| {
        shoal_agents::keep_awake::run(manager.agents.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "idle wakes", |manager| {
        idle_wakes::run(manager.agents.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "idle sleep", idle_sleep::run);
    spawn_supervised(&mut background, &manager, "wake timers", |manager| {
        wake_timers::run(manager.agents.clone())
    });
    spawn_supervised(
        &mut background,
        &manager,
        "shutdown lock",
        shutdown_lock::run,
    );
    spawn_supervised(&mut background, &manager, "sleep lock", sleep_lock::run);
    spawn_supervised(&mut background, &manager, "agent queue", |manager| {
        queued_starts::run(manager.agents.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "triggers", |manager| {
        shoal_runner::triggers::run(manager.runner.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "webhooks", |manager| {
        shoal_runner::triggers::serve_webhooks(manager.runner.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "turn retries", |manager| {
        turn_retry::run(manager.agents.clone())
    });
    spawn_supervised(
        &mut background,
        &manager,
        "interrupted moves",
        moves::finish_interrupted,
    );
    spawn_supervised(&mut background, &manager, "lost hook reports", |manager| {
        lost_reports::run(manager.agents.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "turn ends", turn_ends::run);
    spawn_supervised(
        &mut background,
        &manager,
        "sequential restarts",
        |manager| restarts::run_line(manager.agents.clone(), manager),
    );
    spawn_supervised(
        &mut background,
        &manager,
        "progress sections",
        progress::run,
    );
    spawn_supervised(&mut background, &manager, "agent assignees", |manager| {
        shoal_links::issue::agent_settles::run(manager.links.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "reviews", |manager| {
        shoal_reviews::run(manager.reviews.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "gate", |manager| {
        shoal_reviews::gate::run(manager.reviews.clone(), manager)
    });
    spawn_supervised(
        &mut background,
        &manager,
        "simulator cleanup",
        expire_simulators,
    );
    spawn_supervised(&mut background, &manager, "seed builds", |manager| {
        shoal_repositories::seed::builds::run(manager.repositories.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "seed spares", |manager| {
        shoal_repositories::seed::builds::run_spares(manager.repositories.clone(), manager)
    });
    spawn_supervised(&mut background, &manager, "runner", |manager| {
        shoal_runner::jobs::run(manager.runner.clone(), manager)
    });
    // A foreground daemon never restarts on a replaced binary.
    let restarts_on_update = update.is_some();
    spawn_supervised(&mut background, &manager, "hub", move |manager| {
        shoal_hub::run(manager.hub.clone(), manager, restarts_on_update)
    });
    spawn_supervised(
        &mut background,
        &manager,
        "item activity",
        poll_item_activity,
    );
    let (listener, mut clients) = startup.finish(server.clone()).await;
    let mut quiescence = None;
    let mut requested_exit = None;
    // A signal may come from a shutdown or a logout, so it saves work.
    let result = loop {
        tokio::select! {
            _ = terminate.recv() => break Ok(Ending::Save),
            _ = interrupt.recv() => break Ok(Ending::Save),
            Some(requested) = exit_requested.recv() => {
                let ending = requested.ending;
                requested_exit = Some(requested);
                break Ok(ending);
            }
            _ = update_tick.tick(), if update.is_some() => {
                if update.as_ref().is_some_and(auto_update::Update::pending)
                    && let Some(guard) = exit_wait::quiesce(&manager)
                {
                    quiescence = Some(guard);
                    break Ok(Ending::Detach);
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
                // An exit's attempt to quiesce holds operations off for a
                // moment, and one that waited until the daemon has exited,
                // so the loop never waits for one.
                let operation = server.manager.gates.try_owned_operation();
                clients.spawn(serve_unless_exiting(stream, server, operation));
            }
        }
    };
    // A daemon that stops accepting connections fails and leaves work
    // running for the next daemon the service manager starts. Background
    // loops run on meanwhile, so a stop the shutdown lock started finishes.
    // An exit that waited and quiesced only now finds no daemon to end,
    // so its hold on operations goes at once.
    exit_requested.close();
    while exit_requested.try_recv().is_ok() {}
    let ending = *result.as_ref().unwrap_or(&Ending::Detach);
    if let Some(requested) = &mut requested_exit {
        requested.ending_work();
    }
    let mut stopped = manager.end_work(ending).await;
    manager.record_exit(ending, result.as_ref().err()).await;
    background.shutdown().await;
    // Aborting a run kills its process group; what it started elsewhere
    // carries its marker.
    manager.reviews.stop_runs().await;
    manager.stop_remaining_work(&mut stopped).await;
    if let Some(requested) = &mut requested_exit {
        requested.answer(&manager, &stopped).await;
    }
    clients.shutdown().await;
    manager.store.shutdown().await;
    if quiescence.is_some() {
        log!("daemon executable was updated; restarting");
        return ownership.hand_off(&update.expect("update monitor enabled").path, &listener);
    }
    result.map(|_| ())
}

/// Open the daemon's state and reconcile it with what changed while it was
/// not running.
async fn start(startup: &startup::Startup, paths: &Paths) -> Result<Arc<Manager>> {
    let manager = startup
        .step("opening the state database", Manager::open(paths.clone()))
        .await?;
    startup.follow(manager.file_access.clone());
    startup
        .step(
            "quarantining interrupted operations",
            crate::daemon::quarantine::interrupted_operations(&manager.store),
        )
        .await?;
    // Finishing a move restores the states quarantine failed.
    let moves = manager.repositories.finish_moves(&*manager);
    if let Err(error) = startup.step("finishing repository moves", moves).await {
        log!("{error:#}");
    }
    let hosts = manager.learn_recorded_forge_hosts();
    startup.step("learning forge hosts", hosts).await?;
    let audit = manager.audit_worktrees();
    startup.step("auditing worktrees", audit).await?;
    // Before requests, so no surviving start worker finds its readings held.
    let recovery_starts = manager.agents.return_recovery_starts();
    (startup.step(
        "requeueing restores admitted on old readings",
        recovery_starts,
    ))
    .await?;
    let removals = manager.restore_interrupted_removals();
    if let Err(error) = startup
        .step("restoring files of interrupted removals", removals)
        .await
    {
        log!("{error:#}");
    }
    let reviews = manager.reviews.recover(&*manager);
    if let Err(error) = startup
        .step("removing interrupted reviews' checkouts", reviews)
        .await
    {
        log!("{error:#}");
    }
    let imports = manager.remove_interrupted_imports();
    if let Err(error) = startup
        .step("removing interrupted imports' packages", imports)
        .await
    {
        log!("{error:#}");
    }
    let gate = manager.reviews.recover_gate_runs(&*manager);
    if let Err(error) = startup.step("stopping interrupted gate runs", gate).await {
        log!("{error:#}");
    }
    let sessions = manager.agents.cleanup_orphan_sessions(&*manager);
    startup.step("cleaning up orphan sessions", sessions).await;
    Ok(manager)
}

impl Manager {
    /// Record in Shoal's log that the daemon started.
    async fn record_start(&self) {
        let mut started = ShoalEventDetails::new(ShoalEventKind::DaemonStarted);
        started.version = Some(env!("CARGO_PKG_VERSION").to_owned());
        crate::daemon::events::record_shoal(&self.store, started).await;
    }

    /// Bring the installed service definition up to date with this release,
    /// as the daemon its service manager started does, telling the user
    /// when it cannot.
    async fn refresh_service_definition(&self) {
        match crate::service::refresh(&self.paths).await {
            Ok(true) => log!("updated the installed service definition"),
            Ok(false) => {}
            Err(error) => {
                log!("service definition not updated: {error:#}");
                let message = format!(
                    "Shoal could not update its installed service definition: {error:#}; run `shoal daemon install`"
                );
                self.notify(None, AlertKind::EnvironmentProblem, message)
                    .await;
            }
        }
    }

    /// Record in Shoal's log that the daemon exits with `ending`, after
    /// `error` when it failed: a restart where the next daemon takes over
    /// running work, else a stop.
    async fn record_exit(&self, ending: Ending, error: Option<&anyhow::Error>) {
        let kind = match (ending, error) {
            (Ending::Detach, None) => ShoalEventKind::DaemonRestarted,
            _ => ShoalEventKind::DaemonStopped,
        };
        crate::daemon::events::record_shoal_outcome(&self.store, kind, error).await;
    }
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
        tokio::select! {
            () = manager.leases.simulator_idled() => {}
            () = tokio::time::sleep(SIMULATOR_SWEEP_INTERVAL) => {}
        }
        let _operation = manager.gates.background_pass().await;
        if let Err(error) = manager.leases.expire_simulators().await {
            log!("simulator cleanup: {error:#}");
        }
    }
}

async fn poll_item_activity(manager: Arc<Manager>) {
    let mut interval = tokio::time::interval(ITEM_ACTIVITY_SWEEP_INTERVAL);
    loop {
        interval.tick().await;
        let _operation = manager.gates.operation().await;
        // Blockers are unrelated items; a slow item sweep must not delay them.
        let (items, blockers) = tokio::join!(
            manager.links.poll_linked_item_activity(&*manager),
            manager.poll_blockers(),
        );
        if let Err(error) = items {
            log!("item activity: {error:#}");
        }
        if let Err(error) = blockers {
            log!("blockers: {error:#}");
        }
        if let Err(error) = manager.links.pass_up_delegated_activity(&*manager).await {
            log!("item activity for coordinators elsewhere: {error:#}");
        }
    }
}

/// Serve one connection under `operation`, or under one taken once a
/// passing attempt to quiesce lets go, or refuse it once an exit that
/// waited holds operations off until the daemon has exited.
async fn serve_unless_exiting(
    stream: UnixStream,
    server: Server,
    operation: Option<OperationGuard>,
) {
    let gates = &server.manager.gates;
    let operation = match operation {
        Some(operation) => Some(operation),
        None => gates.operation_unless_exiting().await,
    };
    let Some(operation) = operation else {
        return connections::refuse_while_exiting(stream, gates.exiting()).await;
    };
    if let Err(error) = serve(stream, server, operation).await {
        log!("client connection: {error:#}");
    }
}

/// Answer one connection's request.
async fn serve(
    mut stream: UnixStream,
    server: Server,
    operation_guard: OperationGuard,
) -> Result<()> {
    match read_request(&mut stream).await? {
        Some(request) => serve_request(stream, server, request, operation_guard).await,
        None => Ok(()),
    }
}

/// Admit a connection's request, resume an execution or accept it, then
/// answer it.
async fn serve_request(
    mut stream: UnixStream,
    server: Server,
    request: Request,
    operation_guard: OperationGuard,
) -> Result<()> {
    let _span = request_span(&request);
    let Some(_slot) = admit(&mut stream, &server, &request).await? else {
        return Ok(());
    };
    let name = format!("request {}", request.method.name());
    let mut operation = match request.method {
        // Wrappers from any release reattach, and their launch scope does not
        // survive the restart; the record and token they present prove ownership.
        Method::Reattach(Reattachment::Reattach(reattach)) => {
            return execution_connection::reattach(
                stream,
                request.id,
                request.protocol,
                server.manager.agents.clone(),
                server.manager,
                reattach,
                operation_guard,
            )
            .await;
        }
        // A hub member's link, which checks the protocol itself.
        Method::HubLink(Opening::HubLink(hello)) => {
            if request.scope.is_some() {
                let body = Body::error(
                    ErrorCode::OperationFailed,
                    "a space's processes open no hub link; only the user's own terminal or a command it runs, such as over ssh, does",
                );
                return protocol::write(&mut stream, &Response::new(request.id, body)).await;
            }
            // A link lasts as long as its member keeps it open, so it holds
            // no operation, which would hold off every update meanwhile.
            drop(operation_guard);
            let manager = server.manager.clone();
            shoal_hub::accept_socket_link(
                manager.hub.clone(),
                manager,
                stream,
                request.protocol,
                hello,
            )
            .await;
            return Ok(());
        }
        // Service commands end a daemon of any release.
        Method::Exit(exit) => {
            let scope = request.scope.as_ref();
            return requests::daemon::exit(
                stream,
                request.id,
                &server,
                scope,
                exit,
                operation_guard,
            )
            .await;
        }
        Method::Operation(operation) => operation,
    };
    let scope = request.scope.as_ref();
    let routing = elsewhere::Routing::of(&server, &operation, scope)?;
    let accepted = accept(
        &mut stream,
        &server,
        request.id,
        request.protocol,
        scope,
        routing.as_ref(),
        &mut operation,
    );
    let caller = match accepted.await? {
        Accepted::Caller(caller) => caller,
        Accepted::Refused(error) => {
            let failed = elsewhere::Failed {
                error: &error,
                refused: answer::refusal(&error),
                routing,
                caller: None,
                operation: operation_guard,
            };
            return elsewhere::answer(stream, &server, request.id, failed).await;
        }
        Accepted::Elsewhere(targeted) => {
            let failed = elsewhere::Failed {
                error: &targeted.refusal,
                refused: answer::refusal(&targeted.refusal),
                routing,
                caller: Some(targeted.caller),
                operation: operation_guard,
            };
            return elsewhere::answer(stream, &server, request.id, failed).await;
        }
        Accepted::Answered => return Ok(()),
    };
    let reads_only = operation.reads_only();
    // Its scope narrowed what it names to IDs, which every machine reads.
    let routing = elsewhere::Routing::of(&server, &operation, scope)?;
    let answer = requests::operation(&server, operation, caller.as_ref(), scope);
    let answer = log::when_slow(&name, answer);
    let Some(answer) = unless_abandoned(&mut stream, &name, reads_only, answer).await else {
        return Ok(());
    };
    match answer {
        Ok(answer::Answer::Failed(error)) | Err(error) => {
            let failed = elsewhere::Failed {
                error: &error,
                refused: answer::failure(&error),
                routing,
                caller: Some(caller),
                operation: operation_guard,
            };
            elsewhere::answer(stream, &server, request.id, failed).await
        }
        Ok(answer) => (answer.send(stream, request.id, &server.manager, operation_guard)).await,
    }
}

/// The request's answer, or `None` once its client hung up first on a
/// request that only reads state, which cancels it. One that changes state
/// finishes, so a hangup never leaves it half done.
async fn unless_abandoned<T>(
    stream: &mut UnixStream,
    name: &str,
    reads_only: bool,
    answer: impl Future<Output = T>,
) -> Option<T> {
    if !reads_only {
        return Some(answer.await);
    }
    tokio::select! {
        answer = answer => Some(answer),
        () = answer::hangup(stream) => {
            log!("cancelled {name}: its client hung up");
            None
        }
    }
}

/// The client's request, or `None` once an unreadable one was refused.
async fn read_request(stream: &mut UnixStream) -> Result<Option<Request>> {
    let request = timeout(timing::REQUEST_READ_TIMEOUT, protocol::read(stream)).await?;
    match request {
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

/// Whether a request's scope accepted it.
enum Accepted {
    /// Its caller, the user's none, to whose scope it is narrowed.
    Caller(Option<Caller>),
    /// Its scope refused it.
    Refused(anyhow::Error),
    /// Another joined machine runs the space it targets, which this one
    /// refuses as absent.
    Elsewhere(elsewhere::Targeted),
    /// It was answered already, as in another protocol.
    Answered,
}

/// The caller of a request this daemon's protocol reads and its scope
/// allows, narrowing the request to that scope, or why the scope refused
/// it; unless another machine forwarded it here, which leaves `routing`
/// none, first whether another machine runs the space it targets.
async fn accept(
    stream: &mut UnixStream,
    server: &Server,
    request_id: u64,
    protocol: u32,
    scope: Option<&ScopeToken>,
    routing: Option<&elsewhere::Routing<'_>>,
    operation: &mut Operation,
) -> Result<Accepted> {
    if protocol != protocol::VERSION {
        let body = Body::error(
            ErrorCode::ProtocolMismatch,
            format!(
                "the daemon speaks protocol {}, the request {protocol}; restart the daemon with the installed version",
                protocol::VERSION
            ),
        );
        protocol::write(stream, &Response::new(request_id, body)).await?;
        return Ok(Accepted::Answered);
    }
    if let Some(targeted) = elsewhere::targeted(&server.manager, routing, operation).await {
        return Ok(Accepted::Elsewhere(targeted));
    }
    Ok(
        match scope::authorize(&server.manager, scope, operation).await {
            Ok(caller) => Accepted::Caller(caller),
            Err(error) => Accepted::Refused(error),
        },
    )
}

#[cfg(test)]
mod tests;
