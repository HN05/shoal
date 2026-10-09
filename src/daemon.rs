//! The per-user daemon: owns shared state, serves protocol requests over a
//! Unix socket, and runs background cleanup.
pub mod access;
pub mod allocation;
mod auto_update;
mod cleanup;
mod disk;
pub mod doctor;
pub mod events;
mod execution_connection;
mod execution_recovery;
pub(crate) mod handoff;
pub mod notifications;
mod overload;
#[cfg(test)]
mod overload_tests;
pub mod ports;
pub mod recovery;
pub mod resources;
pub mod scope;
pub mod store;
pub mod workspace;

use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use tokio::{
    net::{UnixListener, UnixStream},
    signal::unix::{SignalKind, signal},
    sync::watch,
    task::JoinSet,
    time::timeout,
};

use ports::Acquisition;
use scope::Caller;
use workspace::{ExecutionKind, Manager, StopRecords};

use crate::{
    paths::Paths,
    protocol::{self, Body, DaemonStatus, ErrorCode, Method, Request, Response, timing},
    removal::InspectionPolicy,
};

const MAX_CLIENTS: usize = 128;
const SIMULATOR_SWEEP_INTERVAL: Duration = Duration::from_secs(15);

/// Never unlink the lock file: waiters must all lock the same inode.
struct Ownership {
    paths: Paths,
    _lock: File,
}

impl Drop for Ownership {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.paths.socket);
    }
}

/// Settings fixed for the daemon's lifetime and shared by every connection.
#[derive(Clone)]
struct Server {
    manager: Arc<Manager>,
    started: Instant,
    /// Running under the OS service manager, which owns shutdown.
    managed: bool,
    shutdown: watch::Sender<bool>,
}

pub async fn run(paths: Paths, managed: bool, handoff: Option<handoff::Handoff>) -> Result<()> {
    paths.prepare()?;
    let (lock, inherited_listener) = match handoff {
        Some(handoff) => {
            ensure!(managed, "daemon handoff requires managed mode");
            handoff.verify(&paths)?;
            (handoff.lock, Some(handoff.listener))
        }
        None => (
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(paths.daemon_lock())?,
            None,
        ),
    };
    lock.try_lock_exclusive()
        .context("a daemon already owns this state directory")?;
    if inherited_listener.is_none() {
        remove_stale_socket(&paths)?;
    }
    let ownership = Ownership {
        paths: paths.clone(),
        _lock: lock,
    };
    let manager = Manager::open(paths.clone()).await?;
    manager.store.quarantine_interrupted_operations().await?;
    manager.audit_worktrees().await?;
    let listener = match inherited_listener {
        Some(listener) => UnixListener::from_std(listener).context("inherit daemon socket")?,
        None => UnixListener::bind(&paths.socket).context("bind daemon socket")?,
    };
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let mut update = auto_update::Update::new(managed);
    let mut update_tick = tokio::time::interval(Duration::from_secs(1));
    let server = Server {
        manager: manager.clone(),
        started: Instant::now(),
        managed,
        shutdown,
    };
    eprintln!("shoal daemon listening on {}", paths.socket.display());
    let mut background = JoinSet::new();
    background.spawn(cleanup::run(manager.clone()));
    background.spawn(overload::run(manager.clone()));
    background.spawn(disk::run(manager.clone()));
    background.spawn(expire_simulators(manager.clone()));
    let mut clients = JoinSet::new();
    let mut quiescence = None;
    let result = loop {
        tokio::select! {
            _ = terminate.recv() => break Ok(()),
            _ = interrupt.recv() => break Ok(()),
            _ = shutdown_rx.changed() => break Ok(()),
            _ = update_tick.tick(), if update.is_some() => {
                if update.as_ref().is_some_and(auto_update::Update::pending) {
                    match auto_update::quiesce(&manager).await {
                        Ok(Some(guard)) => {
                            match update.as_mut().expect("update monitor enabled").supports_followers(clients.len()).await {
                                Ok(true) => { quiescence = Some(guard); break Ok(()); }
                                Ok(false) => {},
                                Err(error) => eprintln!("daemon update check: {error:#}"),
                            }
                        }
                        Ok(None) => {},
                        Err(error) => eprintln!("daemon update check: {error:#}"),
                    }
                }
            },
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            accepted = listener.accept(), if clients.len() < MAX_CLIENTS => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(error.into()),
                };
                let server = server.clone();
                let operation = server.manager.background_operations.clone().read_owned().await;
                clients.spawn(async move {
                    if let Err(error) = serve(stream, server, operation).await {
                        eprintln!("client connection: {error:#}");
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
        eprintln!("daemon executable was updated; restarting");
        return handoff::exec(
            &update.expect("update monitor enabled").path,
            &paths,
            &listener,
            &ownership._lock,
        );
    }
    result
}

fn remove_stale_socket(paths: &Paths) -> Result<()> {
    match fs::symlink_metadata(&paths.socket) {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_socket(),
                "refusing to replace non-socket {}",
                paths.socket.display()
            );
            fs::remove_file(&paths.socket)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

async fn expire_simulators(manager: Arc<Manager>) {
    loop {
        tokio::time::sleep(SIMULATOR_SWEEP_INTERVAL).await;
        let _operation = manager.background_operations.read().await;
        if let Err(error) = manager.expire_simulators().await {
            eprintln!("simulator cleanup: {error:#}");
        }
    }
}

async fn serve(
    mut stream: UnixStream,
    server: Server,
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
    // Wrappers from any release reattach, and their launch scope does not
    // survive the restart; the record and token they present prove ownership.
    if let Method::Reattach(reattach) = request.method {
        return execution_connection::reattach(stream, request.id, server.manager, reattach).await;
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
                    parent_execution: caller
                        .and_then(|caller| caller.execution_id().map(str::to_owned)),
                },
            )
            .await;
        }
        Method::WatchNotifications => {
            return watch_notifications(stream, request.id, server.manager, operation_guard).await;
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
        Method::PrWait {
            workspace,
            timeout_secs,
        } => {
            return wait_pr_updates(stream, request.id, server.manager, workspace, timeout_secs)
                .await;
        }
        Method::WatchItems {
            workspace,
            selection,
            timeout_secs,
        } => {
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
        Method::Shutdown if server.managed => Body::error(
            ErrorCode::ManagedService,
            "stop the daemon through the OS service manager",
        ),
        Method::Shutdown => {
            protocol::write(&mut stream, &Response::new(request.id, Body::Ok)).await?;
            let _ = server.shutdown.send(true);
            return Ok(());
        }
        method => match operation(&server.manager, method, caller.as_ref()).await {
            Ok(body) => body,
            Err(error) => Body::error(ErrorCode::OperationFailed, format!("{error:#}")),
        },
    };
    protocol::write(&mut stream, &Response::new(request.id, body)).await
}

/// Request/response operations. Errors become `operation_failed` replies.
async fn operation(manager: &Manager, method: Method, caller: Option<&Caller>) -> Result<Body> {
    Ok(match method {
        Method::Status
        | Method::Shutdown
        | Method::Execute { .. }
        | Method::Reattach(_)
        | Method::WatchNotifications
        | Method::WatchWorkspaceEvents { .. }
        | Method::PrWait { .. }
        | Method::WatchItems { .. } => {
            anyhow::bail!("unsupported operation")
        }
        Method::ReloadConfig => {
            manager.reload_config().await?;
            Body::Ok
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
        } => Body::OpenedWorkspace(
            manager
                .open_branch(&repository, &branch, git_profile.as_deref(), path, base)
                .await?,
        ),
        Method::CreateWorkspace {
            path,
            repository,
            name,
            base,
            git_profile,
        } => Body::Workspace(
            manager
                .create_workspace(&repository, name, base, git_profile.as_deref(), path)
                .await?,
        ),
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
        Method::ListWorkspaces => {
            let mut workspaces = manager.list_workspaces().await?;
            if let Some(caller) = caller {
                workspaces.retain(|w| w.id == caller.workspace_id);
            }
            for workspace in &mut workspaces {
                manager.annotate_review(workspace).await;
            }
            Body::Workspaces(workspaces)
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
        Method::SetBaseWorkspace { workspace, base } => {
            Body::Workspace(manager.set_base_workspace(&workspace, base).await?)
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
        Method::FindWorkspaces { target } => Body::Workspaces(
            manager
                .find_workspaces(&target, caller.map(|c| c.workspace_id.as_str()))
                .await?,
        ),
        Method::WorkspaceUndone { workspace } => {
            Body::WithdrawnCompletion(manager.undo_done(&workspace).await?)
        }
        Method::AcknowledgePrUpdates {
            workspace,
            deliveries,
        } => {
            manager
                .acknowledge_pr_updates(&workspace, deliveries)
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
        } => Body::ReviewMarks(manager.clear_ready(&workspace, selection).await?),
        Method::WorkspaceDone { workspace, cleanup } => {
            Body::Completion(manager.mark_done(&workspace, cleanup).await?)
        }
        Method::SetIssue { workspace, url } => {
            manager.set_issue(&workspace, &url).await?;
            Body::Ok
        }
        Method::ClearIssue { workspace, url } => {
            manager.clear_issue(&workspace, url.as_deref()).await?;
            Body::Ok
        }
        Method::SetPr { workspace, action } => {
            manager.set_pr(&workspace, action).await?;
            Body::Ok
        }
        Method::StopWorkspace { workspace } => {
            manager
                .stop_workspace(&workspace, StopRecords::Save)
                .await?;
            Body::Ok
        }
        Method::CheckRemoval {
            workspace,
            caller_pid,
            include_changes,
        } => {
            let mut check = manager
                .check_removal(&workspace, removal_inspection(caller_pid))
                .await?;
            if include_changes && check.dirty {
                check.load_changed_files().await?;
            }
            Body::RemovalCheck(check)
        }
        Method::RemoveWorkspace {
            workspace,
            choice,
            caller_pid,
        } => Body::RemovalResult(
            manager
                .remove_workspace(&workspace, choice, removal_inspection(caller_pid))
                .await?,
        ),
        Method::Diagnose => Body::Diagnostics(manager.diagnose().await?),
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
            manager.send_message(&workspace, message).await?;
            Body::Ok
        }
        Method::MarkNotificationsRead { ids } => {
            manager.mark_notifications_read(ids).await?;
            Body::Ok
        }
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
        Method::ListAccess { workspace } => {
            Body::AccessRequests(manager.access_requests(workspace.as_deref()).await?)
        }
        Method::DecideAccess { id, approve } => {
            Body::AccessRequest(Box::new(manager.decide_access(id, approve).await?))
        }
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
        Method::SimCatalog => Body::SimCatalog(manager.simulator_catalog().await?),
        Method::SimOverview { workspace } => {
            Body::SimOverview(manager.simulator_overview(workspace.as_deref()).await?)
        }
        Method::SimList { workspace } => {
            let owner = manager.workspace_filter(workspace.as_deref()).await?;
            Body::Simulators(manager.list_simulators(owner.as_deref()).await?)
        }
        Method::SimAcquire { workspace, request } => {
            let execution_id = caller.and_then(|c| c.execution_id().map(str::to_owned));
            manager
                .acquire_simulator(&workspace, request, execution_id)
                .await?
                .into_body(|sim| Body::Simulator(*sim))
        }
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

/// Long-lived notification stream: the unread backlog, then each new
/// notification as it is recorded, until the client hangs up.
async fn watch_notifications(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    startup: tokio::sync::OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let mut changed = manager.notifications_changed.subscribe();
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    drop(startup);
    let (mut reader, mut writer) = stream.split();
    let mut delivered = 0;
    loop {
        let operation = manager.background_operations.read().await;
        let batch = manager.notifications_after(delivered, 100).await?;
        for notification in batch {
            delivered = notification.id;
            let response = Response::new(request_id, Body::Notification(notification));
            protocol::write(&mut writer, &response).await?;
            manager.mark_notifications_read(vec![delivered]).await?;
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

async fn watch_workspace_events(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    since: Option<i64>,
    follow: bool,
    startup: tokio::sync::OwnedRwLockReadGuard<()>,
) -> Result<()> {
    use crate::daemon::events::EventItem;
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
            match &item {
                EventItem::Event(event) => {
                    if !follow && event.id > end {
                        break;
                    }
                    cursor = Some(event.id);
                    delivered = true;
                }
                EventItem::Gap { oldest_id, .. } => cursor = Some(oldest_id - 1),
            }
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
    timeout_secs: u64,
) -> Result<()> {
    watch_item_updates(
        stream,
        request_id,
        manager,
        workspace,
        crate::forge::link::Selection {
            kind: Some(crate::forge::link::ItemKind::Pr),
            input: None,
        },
        timeout_secs,
    )
    .await
}

async fn watch_item_updates(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    workspace: String,
    selection: crate::forge::link::Selection,
    timeout_secs: u64,
) -> Result<()> {
    let mut closed = [0u8; 1];
    let result = tokio::select! {
        result = manager.wait_items(&workspace, &selection, timeout_secs) => result,
        _ = tokio::io::AsyncReadExt::read(&mut stream, &mut closed) => return Ok(()),
    };
    let body = match result {
        Ok(updates) => Body::PrUpdates(updates),
        Err(error) => Body::error(ErrorCode::OperationFailed, format!("{error:#}")),
    };
    protocol::write(&mut stream, &Response::new(request_id, body)).await
}

/// Preserve the legacy request field at the protocol boundary. Its numeric
/// value has never been used as a PID by removal inspection.
fn removal_inspection(caller_pid: u32) -> InspectionPolicy {
    match caller_pid {
        0 => InspectionPolicy::IncludeDirectoryProcesses,
        _ => InspectionPolicy::GitOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_removal_request_selects_inspection_without_using_a_pid() {
        assert_eq!(
            removal_inspection(0),
            InspectionPolicy::IncludeDirectoryProcesses
        );
        for caller_pid in [1, 123, u32::MAX] {
            assert_eq!(removal_inspection(caller_pid), InspectionPolicy::GitOnly);
        }
    }

    #[tokio::test]
    async fn reattachment_is_served_at_every_protocol_version_without_scope() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (_root, manager) = crate::test_support::manager().await;
        let wrapper = crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap();
        // The spelling an older wrapper sends, with its own protocol version
        // and a scope token the restarted daemon has never issued.
        let request = serde_json::json!({
            "protocol": 1, "id": 3, "scope": "unknown",
            "method": {"reattach": {
                "execution": "missing", "wrapper": wrapper, "child": null,
                "group_id": wrapper.pid, "scope_token": "token"
            }}
        });
        let (client, stream) = UnixStream::pair().unwrap();
        let operation = manager.background_operations.clone().read_owned().await;
        let server = Server {
            manager,
            started: Instant::now(),
            managed: false,
            shutdown: watch::channel(false).0,
        };
        let served = tokio::spawn(serve(stream, server, operation));
        let mut client = BufReader::new(client);
        client
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        timeout(Duration::from_secs(60), client.read_line(&mut response))
            .await
            .unwrap()
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["id"], 3);
        assert_eq!(response["data"]["code"], "execution_failed", "{response}");
        assert!(
            response["data"]["message"]
                .as_str()
                .unwrap()
                .contains("execution record is missing"),
            "{response}"
        );
        served.await.unwrap().unwrap();
    }
}
