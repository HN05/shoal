//! The per-user daemon: owns shared state, serves protocol requests over a
//! Unix socket, and runs background cleanup.
pub mod access;
pub mod allocation;
mod cleanup;
pub mod doctor;
pub mod notifications;
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

use notifications::NotificationKind;
use ports::Acquisition;
use scope::Caller;
use workspace::{ExecutionKind, Manager, StartedExecution};

use crate::{
    paths::Paths,
    process::identity::Identity,
    protocol::{
        self, Body, Control, DaemonStatus, ErrorCode, ExecutionEvent, Method, Request, Response,
        timing,
    },
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

pub async fn run(paths: Paths, managed: bool) -> Result<()> {
    paths.prepare()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(paths.daemon_lock())?;
    lock.try_lock_exclusive()
        .context("a daemon already owns this state directory")?;
    remove_stale_socket(&paths)?;
    let _ownership = Ownership {
        paths: paths.clone(),
        _lock: lock,
    };
    let manager = Manager::open(paths.clone()).await?;
    manager.store.quarantine_interrupted_operations().await?;
    manager.audit_worktrees().await?;
    let listener = UnixListener::bind(&paths.socket).context("bind daemon socket")?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let server = Server {
        manager: manager.clone(),
        started: Instant::now(),
        managed,
        shutdown,
    };
    eprintln!("shoal daemon listening on {}", paths.socket.display());
    let mut background = JoinSet::new();
    background.spawn(cleanup::run(manager.clone()));
    background.spawn(expire_simulators(manager.clone()));
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
            _ = shutdown_rx.changed() => break,
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            accepted = listener.accept(), if clients.len() < MAX_CLIENTS => {
                let (stream, _) = accepted?;
                let server = server.clone();
                clients.spawn(async move {
                    if let Err(error) = serve(stream, server).await {
                        eprintln!("client connection: {error:#}");
                    }
                });
            }
        }
    }
    background.shutdown().await;
    clients.shutdown().await;
    Ok(())
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
        if let Err(error) = manager.expire_simulators().await {
            eprintln!("simulator cleanup: {error:#}");
        }
    }
}

async fn serve(mut stream: UnixStream, server: Server) -> Result<()> {
    let mut request: Request =
        match timeout(timing::REQUEST_READ_TIMEOUT, protocol::read(&mut stream)).await? {
            Ok(request) => request,
            Err(error) => {
                let response = Response::new(0, Body::error(ErrorCode::InvalidRequest, error));
                return protocol::write(&mut stream, &response).await;
            }
        };
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
        } => {
            return execute(
                stream,
                request.id,
                server.manager,
                ExecutionContext {
                    workspace,
                    wrapper,
                    kind,
                    agent,
                    parent_execution: caller.map(|caller| caller.execution_id),
                },
            )
            .await;
        }
        Method::WatchNotifications => {
            return watch_notifications(stream, request.id, server.manager).await;
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
        Method::Status | Method::Shutdown | Method::Execute { .. } | Method::WatchNotifications => {
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
            key,
            value,
        } => Body::RepositoryConfig(
            manager
                .edit_repository_config(&repository, &key, value.as_deref())
                .await?,
        ),
        Method::RemoveRepository { repository } => {
            Body::RepositoryRemoved(manager.remove_repository(&repository).await?)
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
        Method::AdoptWorkspace { repository, path } => {
            Body::Workspace(manager.adopt_workspace(&repository, &path).await?)
        }
        Method::ListWorkspaces => {
            let mut workspaces = manager.list_workspaces().await?;
            if let Some(caller) = caller {
                workspaces.retain(|w| w.id == caller.workspace_id);
            }
            Body::Workspaces(workspaces)
        }
        Method::InspectWorkspace { workspace } => {
            Body::Inspection(manager.inspect_workspace(&workspace).await?)
        }
        Method::WorkspaceStatus { workspace } => {
            Body::WorkspaceStatus(manager.workspace_status(&workspace).await?)
        }
        Method::SetPr { workspace, action } => {
            manager.set_pr(&workspace, action).await?;
            Body::Ok
        }
        Method::StopWorkspace { workspace } => {
            manager.stop_workspace(&workspace).await?;
            Body::Ok
        }
        Method::CheckRemoval {
            workspace,
            caller_pid,
        } => Body::RemovalCheck(
            manager
                .check_removal(&workspace, removal_inspection(caller_pid))
                .await?,
        ),
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
                caller.is_some_and(|caller| caller.kind == ExecutionKind::Land),
                "landing execution required"
            );
            Body::Ok
        }
        Method::RefreshMergeSource { workspace, branch } => {
            Body::PulledBranch(manager.refresh_merge_source(&workspace, &branch).await?)
        }
        Method::DiffBase { workspace } => Body::DiffBase(manager.diff_base(&workspace).await?),
        Method::WorkspaceHook { workspace, kind } => {
            let workspace = manager.workspace(&workspace).await?;
            Body::Hook(manager.workspace_hook(&workspace, kind).await?)
        }
        Method::LayeredConfig { target } => {
            Body::LayeredConfig(Box::new(manager.config_layers_for(target).await?))
        }
        Method::CommandLayers { workspace } => {
            Body::CommandLayers(manager.command_layers(&workspace).await?)
        }
        Method::ListNotifications { unread_only, limit } => {
            Body::Notifications(manager.notifications(unread_only, limit).await?)
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
            let execution_id = caller.map(|c| c.execution_id.clone());
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
) -> Result<()> {
    let mut changed = manager.notifications_changed.subscribe();
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    let (mut reader, mut writer) = stream.split();
    let mut delivered = 0;
    loop {
        let batch = manager.notifications_after(delivered, 100).await?;
        for notification in batch {
            delivered = notification.id;
            let response = Response::new(request_id, Body::Notification(notification));
            protocol::write(&mut writer, &response).await?;
            manager.mark_notifications_read(vec![delivered]).await?;
        }
        let mut closed = [0u8; 1];
        tokio::select! {
            recorded = changed.changed() => recorded?,
            // The client never writes; a read completes only when it hangs up.
            _ = tokio::io::AsyncReadExt::read(&mut reader, &mut closed) => return Ok(()),
        }
    }
}

struct ExecutionContext {
    workspace: String,
    wrapper: Identity,
    kind: ExecutionKind,
    agent: Option<String>,
    parent_execution: Option<String>,
}

/// Long-lived execution connection: register the wrapper's child, relay stop
/// requests, and record completion when the wrapper reports it.
async fn execute(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    context: ExecutionContext,
) -> Result<()> {
    let ExecutionContext {
        workspace,
        wrapper,
        kind,
        agent,
        parent_execution,
    } = context;
    let StartedExecution {
        plan,
        mut stop,
        _git_guard,
    } = match manager
        .begin_execution(&workspace, Some(wrapper), kind, parent_execution.as_deref())
        .await
    {
        Ok(begun) => begun,
        Err(error) => {
            let body = Body::error(ErrorCode::ExecutionFailed, format!("{error:#}"));
            return protocol::write(&mut stream, &Response::new(request_id, body)).await;
        }
    };
    let execution_id = plan.id.clone();
    let workspace_name = plan.workspace.name.clone();
    let (mut reader, mut writer) = stream.split();
    let result = async {
        protocol::write(
            &mut writer,
            &Response::new(request_id, Body::Execution(plan)),
        )
        .await?;
        match protocol::read::<ExecutionEvent>(&mut reader).await? {
            ExecutionEvent::Finished { exit_code } => return Ok(exit_code),
            ExecutionEvent::Started { child, group_id } => {
                manager
                    .record_execution_child(execution_id.clone(), child, group_id)
                    .await?;
                protocol::write(&mut writer, &Control::Started).await?;
            }
        }
        let finished = protocol::read::<ExecutionEvent>(&mut reader);
        tokio::pin!(finished);
        let mut sent_stop = false;
        loop {
            tokio::select! {
                result = &mut finished => break match result? {
                    ExecutionEvent::Finished { exit_code } => Ok(exit_code),
                    _ => anyhow::bail!("unexpected execution event"),
                },
                changed = stop.changed(), if !sent_stop => {
                    changed?;
                    protocol::write(&mut writer, &Control::Stop).await?;
                    sent_stop = true;
                }
            }
        }
    }
    .await;
    let complete = manager
        .finish_execution(execution_id, kind, result.as_ref().ok().copied())
        .await?;
    if let Some(agent) = agent {
        let message = match &result {
            Ok(code) if complete => format!("{agent} exited with code {code}"),
            Ok(code) => format!(
                "{agent} exited with code {code}, leaving processes behind; run shoal doctor"
            ),
            Err(_) => format!("{agent} disconnected without reporting; run shoal doctor"),
        };
        manager
            .notify(
                Some(&workspace_name),
                NotificationKind::AgentExited,
                message,
            )
            .await;
    }
    if result.is_ok() {
        protocol::write(&mut writer, &Control::Finished { complete }).await?;
    }
    result.map(|_| ())
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
}
