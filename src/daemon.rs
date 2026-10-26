//! The per-user daemon: owns shared state, serves protocol requests over a
//! Unix socket, and runs background cleanup.
pub mod access;
pub mod agent_messages;
pub(crate) mod agent_queue;
pub mod allocation;
pub mod approvals;
mod auto_update;
pub(crate) mod caches;
pub mod chat_messages;
mod chats;
pub mod cleanup;
mod connections;
pub(crate) mod cpu_usage;
mod disk;
pub mod doctor;
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
pub mod resources;
mod restart_recovery;
pub mod scope;
pub mod secrets;
pub(crate) mod seed;
pub mod store;
mod warnings;
pub mod workspace;
mod workspace_usage;

use std::{
    panic::AssertUnwindSafe,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, ensure};
use futures_util::FutureExt;
use tokio::{
    net::UnixStream,
    signal::unix::{SignalKind, signal},
    sync::watch,
    task::JoinSet,
    time::timeout,
};

pub(crate) use log::log;
use ownership::Ownership;
use ports::Acquisition;
use scope::Caller;
use workspace::{ExecutionKind, Manager, StopRequest};

use crate::{
    paths::Paths,
    protocol::{self, Body, DaemonStatus, ErrorCode, Method, Request, Response, timing},
    removal::InspectionPolicy,
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
    tokio::spawn({
        let manager = manager.clone();
        async move { manager.report_environment().await }
    });
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
            accepted = server.capacity.accept(&listener) => {
                let (stream, slot) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(error.into()),
                };
                let server = server.clone();
                let operation = server.manager.background_operations.clone().read_owned().await;
                clients.spawn(async move {
                    if let Err(error) = serve(stream, server, slot, operation).await {
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
        let _operation = manager.background_operations.read().await;
        if let Err(error) = manager.expire_simulators().await {
            log!("simulator cleanup: {error:#}");
        }
    }
}

async fn poll_item_activity(manager: Arc<Manager>) {
    let mut interval = tokio::time::interval(ITEM_ACTIVITY_SWEEP_INTERVAL);
    loop {
        interval.tick().await;
        let _operation = manager.background_operations.read().await;
        if let Err(error) = manager.poll_linked_item_activity().await {
            log!("item activity: {error:#}");
        }
        if let Err(error) = manager.poll_dependency_issues().await {
            log!("agent queue dependencies: {error:#}");
        }
    }
}

async fn serve(
    mut stream: UnixStream,
    server: Server,
    slot: connections::Slot,
    operation_guard: tokio::sync::OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let mut request: Request =
        match timeout(timing::REQUEST_READ_TIMEOUT, protocol::read(&mut stream)).await? {
            Ok(request) => request,
            Err(error) => {
                let response = Response::new(0, Body::error(ErrorCode::InvalidRequest, error));
                return protocol::write(&mut stream, &response).await;
            }
        };
    let _span = if request.method.holds_connection() {
        None
    } else {
        crate::profile::span("request", || request.method.name())
    };
    let _slot = match server.capacity.admit(slot, &request.method) {
        Ok(slot) => slot,
        // Their clients take a reply as final but wait for a free slot this way.
        Err(_) if request.method.retries_when_closed() => return Ok(()),
        Err(error) => {
            let body = Body::error(ErrorCode::OperationFailed, format!("{error:#}"));
            return protocol::write(&mut stream, &Response::new(request.id, body)).await;
        }
    };
    // Wrappers from any release reattach, and their launch scope does not
    // survive the restart; the record and token they present prove ownership.
    if let Method::Reattach(reattach) = request.method {
        return execution_connection::reattach(
            stream,
            request.id,
            server.manager,
            reattach,
            operation_guard,
        )
        .await;
    }
    if request.protocol != protocol::VERSION {
        let body = Body::error(
            ErrorCode::ProtocolMismatch,
            "restart the daemon with the installed version",
        );
        return protocol::write(&mut stream, &Response::new(request.id, body)).await;
    }
    let caller = match scope::authorize(
        &server.manager,
        request.scope.as_deref(),
        &mut request.method,
    )
    .await
    {
        Ok(caller) => caller,
        Err(error) => {
            let body = Body::error(ErrorCode::ScopeDenied, format!("{error:#}"));
            return protocol::write(&mut stream, &Response::new(request.id, body)).await;
        }
    };
    let body = match request.method {
        Method::Execute {
            workspace,
            wrapper,
            kind,
            agent,
            recover,
            reattach,
            queue_entry,
            demo,
        } => {
            return execution_connection::execute(
                stream,
                request.id,
                server.manager,
                execution_connection::ExecutionContext {
                    workspace,
                    wrapper,
                    kind,
                    agent,
                    recover,
                    reattach,
                    queue_entry,
                    demo: demo.map(|demo| *demo),
                    caller,
                },
                operation_guard,
            )
            .await;
        }
        Method::WatchNotifications { keep_unread, after } => {
            return watch_notifications(
                stream,
                request.id,
                server.manager,
                keep_unread,
                after,
                operation_guard,
            )
            .await;
        }
        Method::QueueHold { entry } => {
            return agent_queue::hold(stream, request.id, server.manager, entry, operation_guard)
                .await;
        }
        Method::WatchWorkspaceEvents { since, follow } => {
            return watch_workspace_events(
                stream,
                request.id,
                server.manager,
                since,
                follow,
                operation_guard,
            )
            .await;
        }
        // A cancelled wait leaves its updates pending, so an update need not
        // wait for it; clients wait again on the replacement.
        Method::PrWait {
            workspace,
            timeout_secs,
        } => {
            drop(operation_guard);
            return wait_pr_updates(stream, request.id, server.manager, workspace, timeout_secs)
                .await;
        }
        Method::AwaitAnswer { workspace, id, ask } => {
            drop(operation_guard);
            return await_answer(stream, request.id, server.manager, workspace, id, ask).await;
        }
        Method::WaitChatMessages { chat } => {
            drop(operation_guard);
            let manager = server.manager.clone();
            let wait = async move { Ok(Body::PrUpdates(manager.wait_chat_messages(&chat).await?)) };
            return answer_wait(stream, request.id, &server.manager, wait).await;
        }
        Method::AwaitAgentWake { workspace, due } => {
            drop(operation_guard);
            return await_agent_wake(stream, request.id, server.manager, workspace, due).await;
        }
        Method::WatchItems {
            workspace,
            selection,
            timeout_secs,
        } => {
            drop(operation_guard);
            return watch_item_updates(
                stream,
                request.id,
                server.manager,
                workspace,
                selection,
                timeout_secs,
            )
            .await;
        }
        Method::Status => Body::Status(DaemonStatus {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
            uptime_secs: server.started.elapsed().as_secs(),
            managed: server.managed,
            unread_notifications: server.manager.unread_notifications().await?,
        }),
        Method::ReloadConfig => match server.manager.reload_config().await {
            Ok(()) => {
                // The checks can outlast the reply's budget, so they run after it.
                tokio::spawn({
                    let manager = server.manager.clone();
                    async move { manager.report_environment().await }
                });
                Body::Ok
            }
            Err(error) => Body::error(ErrorCode::OperationFailed, format!("{error:#}")),
        },
        Method::Shutdown if server.managed => Body::error(
            ErrorCode::ManagedService,
            "stop the daemon through the OS service manager",
        ),
        Method::Shutdown => {
            protocol::write(&mut stream, &Response::new(request.id, Body::Ok)).await?;
            let _ = server.shutdown.send(true);
            return Ok(());
        }
        Method::ListItems {
            workspace,
            kind,
            search,
        } => {
            let listed = server
                .manager
                .list_items(&workspace, kind, &search, caller.is_some())
                .await;
            return write_pages(stream, request.id, listed, Body::Items).await;
        }
        Method::ForgeApi {
            workspace,
            request: raw,
        } => {
            let response = server
                .manager
                .forge_api(&workspace, &raw, caller.is_some())
                .await
                .map(|response| protocol::text_pieces(&response.to_string()));
            return write_pages(stream, request.id, response, Body::ApiResponse).await;
        }
        Method::CiRunJobs { workspace, run } => {
            let listed = server.manager.ci_run_jobs(&workspace, run).await;
            return write_pages(stream, request.id, listed, Body::CiRunJobs).await;
        }
        Method::ListCiRuns { workspace, filter } => {
            let listed = server.manager.list_ci_runs(&workspace, filter).await;
            return write_pages(stream, request.id, listed, Body::CiRuns).await;
        }
        Method::RemovedWorkspaces => {
            let listed = server.manager.removed_workspaces().await;
            return write_pages(stream, request.id, listed, Body::RemovedWorkspaces).await;
        }
        Method::AgentEnvironments { repository_id } => {
            let listed = server.manager.agent_environments(&repository_id).await;
            return write_pages(stream, request.id, listed, Body::AgentEnvironments).await;
        }
        method => match operation(&server.manager, method, caller.as_ref()).await {
            Ok(body) => body,
            Err(error) => Body::error(error_code(&error), format!("{error:#}")),
        },
    };
    reply(&mut stream, request.id, body).await
}

/// The code of an operation's failure: specific where the caller can act on
/// it, otherwise `operation_failed`.
fn error_code(error: &anyhow::Error) -> ErrorCode {
    if error.downcast_ref::<workspace::UnprovenStop>().is_some() {
        ErrorCode::UnprovenStop
    } else {
        ErrorCode::OperationFailed
    }
}

/// Write the reply to `request_id`, or an error saying it does not fit in a
/// frame; the oversized reply still fails the connection, for the log.
async fn reply(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    request_id: u64,
    body: Body,
) -> Result<()> {
    let Err(error) = protocol::write(stream, &Response::new(request_id, body)).await else {
        return Ok(());
    };
    if let Some(too_large) = error.downcast_ref::<protocol::FrameTooLarge>() {
        let message = format!("the daemon cannot send its response: {too_large}");
        let body = Body::error(ErrorCode::OperationFailed, message);
        protocol::write(stream, &Response::new(request_id, body)).await?;
    }
    Err(error)
}

/// Request/response operations. Errors become `operation_failed` replies.
async fn operation(manager: &Manager, method: Method, caller: Option<&Caller>) -> Result<Body> {
    Ok(match method {
        Method::Status
        | Method::Shutdown
        | Method::ReloadConfig
        | Method::Execute { .. }
        | Method::Reattach(_)
        | Method::WatchNotifications { .. }
        | Method::QueueHold { .. }
        | Method::WatchWorkspaceEvents { .. }
        | Method::PrWait { .. }
        | Method::AwaitAgentWake { .. }
        | Method::AwaitAnswer { .. }
        | Method::WatchItems { .. }
        | Method::WaitChatMessages { .. }
        | Method::ListItems { .. }
        | Method::ForgeApi { .. }
        | Method::CiRunJobs { .. }
        | Method::ListCiRuns { .. }
        | Method::RemovedWorkspaces
        | Method::AgentEnvironments { .. } => {
            anyhow::bail!("unsupported operation")
        }
        Method::ListRepositories => {
            let mut repos = manager.repositories().await?;
            if let Some(caller) = caller {
                let owner = manager.workspace(&caller.workspace_id).await?;
                repos.retain(|r| r.id == owner.repository_id);
            }
            Body::Repositories(repos)
        }
        Method::RegisterRepository { source, name, path } => {
            Body::Repository(manager.register_repository(source, name, path).await?)
        }
        Method::RegisterContribution { source, name } => {
            Body::Repository(manager.register_contribution(source, name).await?)
        }
        Method::RenameRepository { repository, name } => {
            Body::Repository(manager.rename_repository(&repository, name).await?)
        }
        Method::RepositoryConfig { repository } => {
            Body::RepositoryConfig(manager.repository_config(&repository).await?)
        }
        Method::SetRepositoryConfig { repository, toml } => {
            Body::RepositoryConfig(manager.set_repository_config(&repository, toml).await?)
        }
        Method::EditRepositoryConfig {
            repository,
            changes,
        } => Body::RepositoryConfig(
            manager
                .edit_repository_config(&repository, &changes)
                .await?,
        ),
        Method::RemoveRepository { repository } => {
            Body::RepositoryRemoved(manager.remove_repository(&repository).await?)
        }
        Method::PlanRepositoryMoves { root } => {
            Body::Repositories(manager.plan_repository_moves(root.as_deref()).await?)
        }
        Method::MoveRepositories => Body::MovedRepositories(manager.move_repositories().await?),
        Method::SyncRepository { repository } => {
            Body::SyncedRepository(manager.sync_repository(&repository).await?)
        }
        Method::ListBranches { repository } => Body::Branches(manager.branches(&repository).await?),
        Method::OpenBranch {
            path,
            repository,
            branch,
            git_profile,
            base,
            chat,
        } => {
            let mut opened = manager
                .open_branch(&repository, &branch, git_profile.as_deref(), path, base)
                .await?;
            manager.assign_chat(&opened.workspace.id, chat).await?;
            opened.workspace = manager.workspace(&opened.workspace.id).await?;
            Body::OpenedWorkspace(opened)
        }
        Method::CreateWorkspace {
            path,
            repository,
            name,
            base,
            git_profile,
            chat,
        } => {
            let workspace = manager
                .create_workspace(&repository, name, base, git_profile.as_deref(), path)
                .await?;
            manager.assign_chat(&workspace.id, chat).await?;
            Body::Workspace(manager.workspace(&workspace.id).await?)
        }
        Method::ReopenWorkspace { workspace } => {
            Body::OpenedWorkspace(manager.reopen_workspace(&workspace).await?)
        }
        Method::SplitWorkspace {
            workspace,
            name,
            base,
        } => Body::Workspace(manager.split_workspace(&workspace, name, base).await?),
        Method::CheckCreation {
            repository,
            branch,
            existing,
            base,
            git_profile,
            path,
        } => {
            let options = workspace::NewWorktreeOptions {
                git_profile: git_profile.as_deref(),
                path: path.as_deref(),
                base: base.as_deref(),
            };
            manager
                .check_creation(&repository, branch.as_deref(), existing.as_deref(), options)
                .await?;
            Body::Ok
        }
        Method::CheckDependencies { repository, after } => {
            manager.check_dependencies(&repository, after).await?;
            Body::Ok
        }
        Method::AdoptWorkspace {
            repository,
            path,
            copy,
        } => {
            let workspace = if copy {
                manager.copy_workspace(&repository, &path).await?
            } else {
                manager.adopt_workspace(&repository, &path).await?
            };
            Body::Workspace(workspace)
        }
        Method::RenameWorkspace { workspace, branch } => Body::Workspace(
            manager
                .rename_workspace(&workspace, &branch, caller.and_then(|c| c.execution_id()))
                .await?,
        ),
        Method::ListWorkspaces => Body::Workspaces(
            manager
                .visible_workspaces(caller.map(|c| c.workspace_id.as_str()))
                .await?,
        ),
        Method::StartChat {
            repository,
            agent,
            pid,
        } => Body::Chat(manager.start_chat(repository, agent, pid).await?),
        Method::MoveChat { chat, pid } => Body::Chat(manager.move_chat(&chat, pid).await?),
        Method::ListChats => Body::Chats(if caller.is_some() {
            Vec::new()
        } else {
            manager.chats().await?
        }),
        Method::QueueAdd { request } => Body::QueueAdmission(manager.queue_add(request).await?),
        Method::ListQueue => Body::QueueEntries(if caller.is_some() {
            Vec::new()
        } else {
            manager.queue_entries().await?
        }),
        Method::QueuedPlan { entry } => Body::QueuedPlan(manager.queued_plan(&entry).await?),
        Method::QueueRemove { entry } => Body::QueueEntry(manager.queue_remove(&entry).await?),
        Method::QueueAttach { entry, workspace } => {
            manager.queue_attach(&entry, &workspace).await?;
            Body::Ok
        }
        Method::QueueFail { entry, error } => {
            manager.fail_queue_entry(&entry, &error).await?;
            Body::Ok
        }
        Method::HoldAcquire {
            workspace,
            name,
            reason,
        } => Body::Hold(manager.acquire_hold(&workspace, name, reason).await?),
        Method::HoldRelease { workspace, name } => {
            manager.release_hold(&workspace, name).await?;
            Body::Ok
        }
        Method::HoldList { workspace } => Body::Holds(manager.workspace(&workspace).await?.holds),
        Method::ListDemos { workspace } => Body::Demos(manager.demos(workspace.as_deref()).await?),
        Method::DemoProgress { workspace, launch } => {
            Body::DemoProgress(manager.demo_progress(&workspace, &launch).await?)
        }
        Method::StopDemos { workspace, name } => {
            manager.stop_demos(&workspace, name).await?;
            Body::Ok
        }
        Method::SetBaseWorkspace { workspace, base } => {
            Body::Workspace(manager.set_base_workspace(&workspace, base).await?)
        }
        Method::ItemAction {
            workspace,
            kind,
            item,
            action,
        } => Body::Item(manager.item_action(&workspace, kind, item, action).await?),
        Method::VerifyWorkspace {
            workspace,
            on_branch,
        } => {
            let workspace = manager.workspace(&workspace).await?;
            manager.verify_worktree(&workspace).await?;
            if on_branch {
                crate::daemon::links::pr::current_head(&workspace).await?;
            }
            Body::Workspace(workspace)
        }
        Method::FastForwardWorkspace { workspace, branch } => {
            Body::FastForwarded(manager.fast_forward_workspace(&workspace, &branch).await?)
        }
        Method::OpenPull { workspace, options } => {
            let proposer = manager.proposer(caller).await;
            Body::Opened(manager.open_pull(&workspace, options, proposer).await?)
        }
        Method::OpenIssue {
            workspace,
            issue,
            repository,
            link,
        } => {
            let proposer = manager.proposer(caller).await;
            Body::Opened(
                manager
                    .open_issue(&workspace, issue, repository, link, proposer)
                    .await?,
            )
        }
        Method::InspectWorkspace { workspace } => {
            Body::Inspection(manager.inspect_workspace(&workspace).await?)
        }
        Method::WorkspaceEnv { workspace } => {
            Body::WorkspaceEnv(manager.workspace_environment(&workspace).await?)
        }
        Method::RevokeWorkspaceEnv { workspace, token } => {
            manager
                .revoke_workspace_environment(&workspace, token)
                .await?;
            Body::Ok
        }
        Method::WorkspaceStatus { workspace } => {
            Body::WorkspaceStatus(manager.workspace_status(&workspace).await?)
        }
        Method::SelectItems {
            workspace,
            selection,
        } => Body::SelectedItems(manager.select_items(&workspace, &selection).await?),
        Method::CheckLog {
            workspace,
            selection,
            check,
        } => Body::CheckLog(manager.check_log(&workspace, &selection, &check).await?),
        Method::CiAction {
            workspace,
            item,
            action,
            checks,
        } => Body::CiActed(manager.ci_action(&workspace, item, action, checks).await?),
        Method::CiRunAction {
            workspace,
            run,
            action,
            failed,
        } => Body::CiRunActed(
            manager
                .ci_run_action(&workspace, run, action, failed, caller.is_some())
                .await?,
        ),
        Method::ViewCiRun { workspace, run } => {
            Body::CiRun(manager.view_ci_run(&workspace, run).await?)
        }
        Method::CiRunJobLog {
            workspace,
            run,
            job,
        } => Body::CheckLog(manager.ci_run_job_log(&workspace, run, job).await?),
        Method::StartCiRun {
            workspace,
            workflow,
            branch,
            inputs,
        } => Body::CiRunStarted(
            manager
                .start_ci_run(&workspace, workflow, branch, inputs)
                .await?,
        ),
        Method::FindWorkspaces { target } => {
            // Scoped callers find workspaces in their subtree only.
            let within = match caller {
                Some(caller) => Some(manager.subtree(&caller.workspace_id).await?),
                None => None,
            };
            Body::Workspaces(manager.find_workspaces(&target, within).await?)
        }
        Method::WorkspaceUndone { workspace } => {
            Body::Completion(manager.undo_done(&workspace).await?)
        }
        Method::DiscardStoppedWork {
            workspace,
            execution,
        } => Body::Discarded(manager.discard_stopped_work(&workspace, execution).await?),
        Method::AcknowledgePrUpdates {
            workspace,
            deliveries,
            messages,
        } => {
            // Item updates first: a failure then leaves both to be delivered
            // again, while deleted messages could not be.
            manager
                .acknowledge_pr_updates(&workspace, deliveries)
                .await?;
            manager
                .mark_agent_messages_delivered(&workspace, messages)
                .await?;
            Body::Ok
        }
        Method::MarkReady {
            workspace,
            selection,
        } => Body::ReviewMarks(manager.mark_ready(&workspace, selection).await?),
        Method::ClearReady {
            workspace,
            selection,
            draft,
        } => Body::ReviewMarks(manager.clear_ready(&workspace, selection, draft).await?),
        Method::SetAgentState {
            workspace,
            state,
            session,
        } => {
            let execution = caller.and_then(Caller::execution_id).map(str::to_owned);
            Body::AgentStatus(
                manager
                    .set_agent_state(&workspace, state, execution, session)
                    .await?,
            )
        }
        Method::ReportTurnFailure {
            workspace,
            failure,
            session,
        } => {
            let execution = caller.and_then(Caller::execution_id).map(str::to_owned);
            Body::AgentStatus(
                manager
                    .report_turn_failure(&workspace, failure, execution, session)
                    .await?,
            )
        }
        Method::WorkspaceDone { workspace, cleanup } => {
            Body::Completion(manager.mark_done(&workspace, cleanup).await?)
        }
        Method::SetIssue {
            workspace,
            url,
            title,
        } => {
            manager.set_issue(&workspace, &url, title).await?;
            Body::Ok
        }
        Method::LinkItem {
            workspace,
            kind,
            url,
            relation,
            title,
        } => {
            manager
                .link_item(&workspace, kind, &url, relation, title)
                .await?;
            Body::Ok
        }
        Method::UnlinkItems {
            workspace,
            relation,
            kind,
            url,
        } => {
            manager
                .unlink_items(&workspace, relation, kind, url.as_deref())
                .await?;
            Body::Ok
        }
        Method::SetPr { workspace, action } => {
            manager.set_pr(&workspace, action).await?;
            Body::Ok
        }
        Method::StopWorkspace {
            workspace,
            acknowledge_unproven,
        } => {
            if acknowledge_unproven {
                manager
                    .stop_workspace_acknowledged(&workspace, StopRequest::manual())
                    .await?;
            } else {
                manager
                    .stop_workspace(&workspace, StopRequest::manual())
                    .await?;
            }
            Body::Ok
        }
        Method::CheckRemoval {
            workspace,
            include_changes,
        } => {
            let mut check = manager
                .check_removal(&workspace, InspectionPolicy::GitOnly)
                .await?;
            if include_changes && check.dirty {
                check.load_changed_files().await?;
                check.unsaved_files_archived =
                    manager.archives_unsaved_files(&check.workspace).await?;
            }
            Body::RemovalCheck(check)
        }
        Method::RemoveWorkspace { workspace, unsaved } => Body::RemovalResult(
            manager
                .remove_workspace(&workspace, unsaved, InspectionPolicy::GitOnly)
                .await?,
        ),
        Method::Cleanup { dry_run, selection } => {
            Body::Cleanup(cleanup::on_request(manager, dry_run, &selection).await?)
        }
        Method::KeptBranches { repository } => {
            Body::KeptBranches(manager.list_kept_branches(repository.as_deref()).await?)
        }
        Method::DeleteKeptBranches { branches } => {
            Body::KeptBranches(manager.delete_kept_branches(branches).await?)
        }
        Method::RestoreWorkspace { workspace } => {
            Body::RestoredWorkspace(manager.restore_workspace(&workspace).await?)
        }
        Method::ExportWorkspace {
            workspace,
            unsaved,
            file,
        } => Body::ExportedWorkspace(
            manager
                .export_workspace(&workspace, unsaved, InspectionPolicy::GitOnly, file)
                .await?,
        ),
        Method::ImportWorkspace {
            repository,
            bundle,
            prerequisite,
            workspace,
        } => Body::RestoredWorkspace(
            manager
                .import_workspace(
                    &repository,
                    bundle.as_deref(),
                    prerequisite.as_deref(),
                    *workspace,
                )
                .await?,
        ),
        Method::Checkpoint { workspace, name } => {
            Body::Checkpoint(manager.take_checkpoint(&workspace, name).await?)
        }
        Method::Checkpoints { workspace, after } => {
            Body::Checkpoints(manager.checkpoints(&workspace, after).await?)
        }
        Method::RestoreCheckpoint {
            workspace,
            checkpoint,
        } => Body::RestoredCheckpoint(
            manager
                .restore_checkpoint(&workspace, checkpoint, caller)
                .await?,
        ),
        Method::Diagnose => Body::Diagnostics(manager.diagnose().await?),
        Method::ForgeType { host, scheme } => {
            Body::ForgeType(manager.requested_forge_type(&host, &scheme).await?)
        }
        Method::Doctor { workspace, options } => Body::Doctor(
            manager
                .reconcile_workspaces(workspace.as_deref(), options)
                .await?,
        ),
        Method::CheckLanding => {
            ensure!(
                caller.is_some_and(|caller| caller.kind() == Some(ExecutionKind::Land)),
                "landing execution required"
            );
            Body::Ok
        }
        Method::DiffBase { workspace } => Body::DiffBase(manager.diff_base(&workspace).await?),
        Method::Conflicts { workspace, target } => {
            Body::Conflicts(manager.check_conflicts(&workspace, target).await?)
        }
        Method::KeepAgentEnvironment {
            workspace,
            environment,
        } => {
            manager
                .keep_agent_environment(&workspace, environment)
                .await?;
            Body::Ok
        }
        Method::KeepRepositoryAgentEnvironment {
            repository,
            environment,
        } => {
            manager
                .keep_repository_agent_environment(&repository, environment)
                .await?;
            Body::Ok
        }
        Method::ForgetAgentEnvironments { repository_id } => {
            manager.forget_agent_environments(&repository_id).await?;
            Body::Ok
        }
        Method::WorkspaceHook { workspace, kind } => {
            let workspace = manager.workspace(&workspace).await?;
            Body::Hook(manager.workspace_hook(&workspace, kind).await?)
        }
        Method::LayeredConfig { target } => {
            Body::LayeredConfig(Box::new(manager.config_layers_for(target).await?))
        }
        Method::ListNotifications { unread_only, limit } => {
            Body::Notifications(manager.notifications(unread_only, limit).await?)
        }
        Method::SendMessage { workspace, message } => {
            Body::Sent(manager.send_message(&workspace, message).await?)
        }
        Method::WithdrawMessage { workspace, id } => {
            Body::Notification(manager.withdraw_message(&workspace, id).await?)
        }
        Method::SendAgentMessage { workspace, message } => {
            manager.send_agent_message(&workspace, message).await?;
            Body::Ok
        }
        Method::AgentMessages { workspace } => {
            Body::AgentMessages(manager.agent_messages(&workspace).await?)
        }
        Method::AgentUpdates { workspace } => {
            Body::PrUpdates(manager.agent_updates(&workspace).await?)
        }
        Method::ChatUpdates { chat } => Body::PrUpdates(manager.chat_updates(&chat).await?),
        Method::AcknowledgeChatMessages { chat, ids } => {
            manager.mark_chat_messages_delivered(&chat, ids).await?;
            Body::Ok
        }
        Method::MarkAgentMessagesDelivered { workspace, ids } => {
            manager
                .mark_agent_messages_delivered(&workspace, ids)
                .await?;
            Body::Ok
        }
        Method::AskQuestion { workspace, ask } => {
            Body::Question(manager.ask_question(&workspace, ask).await?)
        }
        Method::ListQuestions { workspace } => {
            Body::Questions(manager.questions(workspace.as_deref()).await?)
        }
        Method::ShowQuestion { id } => {
            Body::Question(scope::show_question(manager, caller, id).await?)
        }
        Method::WithdrawApproval { workspace, id } => {
            Body::ApprovalRequest(Box::new(manager.withdraw_approval(&workspace, id).await?))
        }
        Method::WithdrawQuestion {
            workspace,
            id,
            shown,
        } => Body::Question(manager.withdraw_question(&workspace, id, shown).await?),
        Method::AcceptQuestionRecommendation { id, shown } => {
            Body::Question(manager.accept_question_recommendation(id, shown).await?)
        }
        Method::AnswerQuestion { id, answer, shown } => {
            Body::Question(manager.answer_question(id, answer, shown).await?)
        }
        Method::MarkNotificationsRead { ids } => {
            manager.mark_notifications_read(ids).await?;
            Body::Ok
        }
        Method::ClearNotifications => Body::Cleared(manager.clear_notifications().await?),
        Method::PortAcquire {
            workspace,
            name,
            request,
        } => match manager
            .acquire_port(&workspace, name, request, caller)
            .await?
        {
            Acquisition::Allocation(allocation) => allocation.into_body(Body::Port),
            Acquisition::Suggested(proposal) => Body::PortSuggestion(proposal),
        },
        Method::PortRelease { workspace, name } => {
            manager.release_port(&workspace, name).await?;
            Body::Ok
        }
        Method::PortOverview { workspace } => {
            Body::PortOverview(manager.port_overview(&workspace).await?)
        }
        Method::PortCatalog { workspace } => {
            Body::PortCatalog(manager.port_catalog(&workspace).await?)
        }
        Method::ListApprovals { workspace } => {
            Body::ApprovalRequests(manager.approval_requests(workspace.as_deref()).await?)
        }
        Method::ShowApproval { id } => {
            Body::ApprovalRequest(Box::new(scope::show_approval(manager, caller, id).await?))
        }
        Method::SecretAcquire {
            workspace,
            name,
            request,
        } => manager
            .acquire_secret(&workspace, name, request, caller)
            .await?
            .into_body(Body::SecretGrant),
        Method::SecretRelease { workspace, name } => {
            manager.release_secret(&workspace, name).await?;
            Body::Ok
        }
        Method::SecretCatalog { workspace } => {
            Body::SecretCatalog(manager.secret_catalog(&workspace).await?)
        }
        Method::DecideApproval {
            id,
            approve,
            shown,
            details,
        } => {
            let decided = match &caller {
                // A workspace process decides only what was routed to it.
                Some(caller) => {
                    manager
                        .decide_routed_approval(
                            &caller.workspace_id,
                            id,
                            approve,
                            shown.map(|shown| *shown),
                        )
                        .await?
                }
                None => {
                    manager
                        .decide_approval(id, approve, shown.map(|shown| *shown), details)
                        .await?
                }
            };
            Body::ApprovalRequest(Box::new(decided))
        }
        Method::EscalateApproval {
            workspace,
            id,
            shown,
        } => Body::ApprovalRequest(Box::new(
            manager
                .escalate_approval(&workspace, id, shown.map(|shown| *shown))
                .await?,
        )),
        Method::ResourceAcquire { workspace, request } => manager
            .acquire_resource(&workspace, request, caller)
            .await?
            .into_body(Body::ResourceLease),
        Method::ResourceRelease {
            workspace,
            pool,
            name,
        } => {
            manager.release_resource(&workspace, pool, name).await?;
            Body::Ok
        }
        Method::ResourceOverview { workspace } => {
            Body::ResourceOverview(manager.resource_overview(&workspace).await?)
        }
        Method::ResourceCatalog { workspace } => {
            Body::ResourceCatalog(manager.resource_catalog(&workspace).await?)
        }
        Method::SimCatalog { workspace } => {
            Body::SimCatalog(manager.simulator_catalog(&workspace).await?)
        }
        Method::SimOverview { workspace } => {
            Body::SimOverview(manager.simulator_overview(workspace.as_deref()).await?)
        }
        Method::SimList { workspace } => {
            let owner = manager.workspace_filter(workspace.as_deref()).await?;
            Body::Simulators(manager.list_simulators(owner.as_deref()).await?)
        }
        Method::SimAcquire { workspace, request } => manager
            .acquire_simulator(&workspace, request, caller)
            .await?
            .into_body(|sim| Body::Simulator(*sim)),
        Method::SimRelease { workspace, name } => {
            manager.release_simulator(&workspace, name).await?;
            Body::Ok
        }
        Method::SimHistory {
            workspace,
            limit,
            before,
        } => {
            let owner = manager.workspace_filter(workspace.as_deref()).await?;
            Body::SimHistory(manager.clean_history(owner, limit, before).await?)
        }
    })
}

/// Long-lived notification stream: the unread backlog after `after`, then
/// each new notification as it is recorded, until the client hangs up.
async fn watch_notifications(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    keep_unread: bool,
    after: i64,
    startup: tokio::sync::OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let mut changed = manager.notifications_changed.subscribe();
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    drop(startup);
    let (mut reader, mut writer) = stream.split();
    let mut delivered = after;
    loop {
        let operation = manager.background_operations.read().await;
        let batch = manager.notifications_after(delivered, 100).await?;
        for notification in batch {
            delivered = notification.id;
            let response = Response::new(request_id, Body::Notification(notification));
            protocol::write(&mut writer, &response).await?;
            if !keep_unread {
                manager.mark_notifications_read(vec![delivered]).await?;
            }
        }
        drop(operation);
        let mut closed = [0u8; 1];
        tokio::select! {
            recorded = changed.changed() => recorded?,
            // The client never writes; a read completes only when it hangs up.
            _ = tokio::io::AsyncReadExt::read(&mut reader, &mut closed) => return Ok(()),
        }
    }
}

/// Send a listing in as many `page` frames as it needs, then `ok`, so a long
/// listing never exceeds one frame.
async fn write_pages<T: serde::Serialize>(
    mut stream: UnixStream,
    request_id: u64,
    listed: Result<Vec<T>>,
    page: fn(Vec<T>) -> Body,
) -> Result<()> {
    let items = match listed {
        Ok(items) => items,
        Err(error) => {
            let body = Body::error(ErrorCode::OperationFailed, format!("{error:#}"));
            return protocol::write(&mut stream, &Response::new(request_id, body)).await;
        }
    };
    for items in protocol::frame_pages(items)? {
        protocol::write(&mut stream, &Response::new(request_id, page(items))).await?;
    }
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await
}

async fn watch_workspace_events(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    since: Option<i64>,
    follow: bool,
    startup: tokio::sync::OwnedRwLockReadGuard<()>,
) -> Result<()> {
    if since.is_some_and(|id| id < 0) {
        return protocol::write(
            &mut stream,
            &Response::new(
                request_id,
                Body::error(
                    ErrorCode::InvalidRequest,
                    "event cursor must be nonnegative",
                ),
            ),
        )
        .await;
    }
    // Subscribe before the first read so a concurrent commit cannot be missed.
    let mut changed = manager.store.watch_changes();
    let end = manager.latest_workspace_event_id().await?;
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    let _finite_request = if follow {
        drop(startup);
        None
    } else {
        Some(startup)
    };
    let (mut reader, mut writer) = stream.split();
    let mut cursor = since;
    loop {
        let operation = manager.background_operations.read().await;
        let items = manager.workspace_events(cursor, 100).await?;
        let mut delivered = false;
        for item in items {
            if let Some(id) = item.event_id() {
                if !follow && id > end {
                    break;
                }
                delivered = true;
            }
            cursor = Some(item.cursor());
            protocol::write(
                &mut writer,
                &Response::new(request_id, Body::EventItem(item)),
            )
            .await?;
        }
        // Drain the complete backlog before waiting for another commit.
        if delivered && (follow || cursor.is_some_and(|id| id < end)) {
            continue;
        }
        if !follow {
            protocol::write(&mut writer, &Response::new(request_id, Body::Ok)).await?;
            return Ok(());
        }
        drop(operation);
        let mut closed = [0u8; 1];
        tokio::select! {
            result = changed.changed() => result?,
            _ = tokio::io::AsyncReadExt::read(&mut reader, &mut closed) => return Ok(()),
        }
    }
}

async fn wait_pr_updates(
    stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    workspace: String,
    timeout_secs: Option<u64>,
) -> Result<()> {
    watch_item_updates(
        stream,
        request_id,
        manager,
        workspace,
        crate::forge::items::link::Selection {
            kind: Some(crate::forge::items::link::ItemKind::Pr),
            input: None,
        },
        timeout_secs,
    )
    .await
}

async fn watch_item_updates(
    stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    workspace: String,
    selection: crate::forge::items::link::Selection,
    timeout_secs: Option<u64>,
) -> Result<()> {
    let updates = async {
        let updates = manager
            .wait_items(&workspace, &selection, timeout_secs)
            .await?;
        Ok(Body::PrUpdates(updates))
    };
    answer_wait(stream, request_id, &manager, updates).await
}

async fn await_agent_wake(
    stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    workspace: String,
    due: bool,
) -> Result<()> {
    let wake = async {
        manager.wait_agent_wake(&workspace, due).await?;
        Ok(Body::Ok)
    };
    answer_wait(stream, request_id, &manager, wake).await
}

async fn await_answer(
    stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    workspace: String,
    id: i64,
    ask: crate::daemon::questions::Ask,
) -> Result<()> {
    let answer = async {
        let question = manager.await_answer(&workspace, id, &ask).await?;
        Ok(Body::Question(question))
    };
    answer_wait(stream, request_id, &manager, answer).await
}

/// Answer a request that waits on the daemon without holding off its restart.
/// A closed connection cancels the wait. A restarting daemon could cut the
/// reply off, so it closes the connection unanswered instead: the client asks
/// again on the replacement, where the awaited result remains pending.
async fn answer_wait(
    mut stream: UnixStream,
    request_id: u64,
    manager: &Manager,
    wait: impl Future<Output = Result<Body>>,
) -> Result<()> {
    let mut closed = [0u8; 1];
    let result = tokio::select! {
        result = wait => result,
        _ = tokio::io::AsyncReadExt::read(&mut stream, &mut closed) => return Ok(()),
    };
    let body = result
        .unwrap_or_else(|error| Body::error(ErrorCode::OperationFailed, format!("{error:#}")));
    // Hold the operation guard through the write so a restart cannot begin
    // while the reply is in flight.
    let Ok(_operation) = manager.background_operations.try_read() else {
        return Ok(());
    };
    protocol::write(&mut stream, &Response::new(request_id, body)).await
}

#[cfg(test)]
mod tests;
