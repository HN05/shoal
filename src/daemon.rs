//! The per-user daemon: owns shared state, serves protocol requests over a
//! Unix socket, and runs background cleanup.
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

use crate::{
    paths::Paths,
    ports::ReserveOutcome,
    process_identity::Identity,
    protocol::{self, Body, Control, ExecutionEvent, Method, Request, Response, Status},
    scope::Caller,
    workspace::{ExecutionKind, Manager},
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
    let idle = manager
        .config
        .auto_cleanup
        .enabled
        .then(|| Duration::from_secs(manager.config.auto_cleanup.idle_minutes * 60));
    background.spawn(crate::cleanup::run(manager.clone(), idle));
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
        match timeout(Duration::from_secs(5), protocol::read(&mut stream)).await? {
            Ok(request) => request,
            Err(error) => {
                let response = Response::new(0, Body::error("invalid_request", error));
                return protocol::write(&mut stream, &response).await;
            }
        };
    if request.protocol != protocol::VERSION {
        let body = Body::error(
            "protocol_mismatch",
            "restart the daemon with the installed version",
        );
        return protocol::write(&mut stream, &Response::new(request.id, body)).await;
    }
    let caller = match crate::scope::authorize(
        &server.manager,
        request.scope.as_deref(),
        &mut request.method,
    )
    .await
    {
        Ok(caller) => caller,
        Err(error) => {
            let body = Body::error("scope_denied", format!("{error:#}"));
            return protocol::write(&mut stream, &Response::new(request.id, body)).await;
        }
    };
    let body = match request.method {
        Method::Execute { workspace, wrapper } => {
            let kind = ExecutionKind::Command;
            return execute(stream, request.id, workspace, wrapper, server.manager, kind).await;
        }
        Method::Prepare { workspace, wrapper } => {
            let kind = ExecutionKind::Setup;
            return execute(stream, request.id, workspace, wrapper, server.manager, kind).await;
        }
        Method::Status => Body::Status(Status {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
            uptime_secs: server.started.elapsed().as_secs(),
            managed: server.managed,
        }),
        Method::Shutdown if server.managed => Body::error(
            "managed_service",
            "stop the daemon through the OS service manager",
        ),
        Method::Shutdown => {
            protocol::write(&mut stream, &Response::new(request.id, Body::Ok)).await?;
            let _ = server.shutdown.send(true);
            return Ok(());
        }
        method => match operation(&server.manager, method, caller.as_ref()).await {
            Ok(body) => body,
            Err(error) => Body::error("operation_failed", format!("{error:#}")),
        },
    };
    protocol::write(&mut stream, &Response::new(request.id, body)).await
}

/// Request/response operations. Errors become `operation_failed` replies.
async fn operation(manager: &Manager, method: Method, caller: Option<&Caller>) -> Result<Body> {
    Ok(match method {
        Method::Status | Method::Shutdown | Method::Execute { .. } | Method::Prepare { .. } => {
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
        Method::RemoveRepository { repository } => {
            Body::RepositoryRemoved(manager.remove_repository(&repository).await?)
        }
        Method::ListBranches { repository } => Body::Branches(manager.branches(&repository).await?),
        Method::OpenBranch { repository, branch } => {
            Body::OpenedWorkspace(manager.open_branch(&repository, &branch).await?)
        }
        Method::CreateWorkspace {
            repository,
            name,
            base,
        } => Body::Workspace(manager.create_workspace(&repository, name, base).await?),
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
        Method::SetPr {
            workspace,
            url,
            clear,
        } => {
            manager.set_pr(&workspace, url, clear).await?;
            Body::Ok
        }
        Method::StopWorkspace { workspace } => {
            manager.stop_workspace(&workspace).await?;
            Body::Ok
        }
        Method::CheckRemoval {
            workspace,
            caller_pid,
        } => Body::RemovalCheck(manager.check_removal(&workspace, caller_pid).await?),
        Method::RemoveWorkspace {
            workspace,
            choice,
            caller_pid,
        } => Body::RemovalResult(
            manager
                .remove_workspace(&workspace, choice, caller_pid)
                .await?,
        ),
        Method::Reconcile { workspace, options } => Body::Reconciliation(
            manager
                .reconcile_workspaces(workspace.as_deref(), options)
                .await?,
        ),
        Method::PullDefaultBranch { workspace } => {
            Body::PulledBranch(manager.pull_default_branch(&workspace).await?)
        }
        Method::LandWorkspace { workspace } => {
            Body::LandedBranch(manager.land_workspace(&workspace).await?)
        }
        Method::RefreshMergeSource { workspace, branch } => {
            Body::PulledBranch(manager.refresh_merge_source(&workspace, &branch).await?)
        }
        Method::DiffBase { workspace } => Body::DiffBase(manager.diff_base(&workspace).await?),
        Method::WorkspaceHooks { workspace } => {
            Body::Hooks(manager.workspace_hooks(&workspace).await?)
        }
        Method::ReservePort {
            workspace,
            name,
            request,
        } => match manager.reserve_port(&workspace, name, request).await? {
            ReserveOutcome::Reserved(port) => Body::Port(port),
            ReserveOutcome::Suggested(proposal) => Body::PortSuggestion(proposal),
        },
        Method::ReleasePort { workspace, name } => {
            manager.release_port(&workspace, name).await?;
            Body::Ok
        }
        Method::ListPorts { workspace } => {
            Body::Ports(manager.list_ports(workspace.as_deref()).await?)
        }
        Method::PortOverview { workspace } => {
            Body::PortOverview(manager.port_overview(&workspace).await?)
        }
        Method::ResourceAcquire { workspace, request } => {
            match manager.acquire_resource(&workspace, request).await? {
                crate::resources::Acquisition::Acquired(lease) => Body::ResourceLease(lease),
                crate::resources::Acquisition::Busy(message) => Body::ResourceBusy { message },
            }
        }
        Method::ResourceRelease {
            workspace,
            pool,
            name,
        } => {
            manager.release_resource(&workspace, pool, name).await?;
            Body::Ok
        }
        Method::ResourceList { workspace } => {
            Body::ResourceLeases(manager.list_resources(workspace.as_deref()).await?)
        }
        Method::ResourceOverview { workspace } => {
            Body::ResourceOverview(manager.resource_overview(&workspace).await?)
        }
        Method::SimCatalog => Body::SimCatalog(manager.simulator_catalog().await?),
        Method::SimList { workspace } => {
            let owner = manager.workspace_filter(workspace.as_deref()).await?;
            Body::Simulators(manager.list_simulators(owner.as_deref()).await?)
        }
        Method::SimAcquire { workspace, request } => {
            let execution_id = caller.map(|c| c.execution_id.clone());
            match manager
                .acquire_simulator(&workspace, request, execution_id)
                .await?
            {
                crate::simulators::Acquisition::Acquired(sim) => Body::Simulator(*sim),
                crate::simulators::Acquisition::Busy(message) => Body::SimBusy { message },
            }
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

/// Long-lived execution connection: register the wrapper's child, relay stop
/// requests, and record completion when the wrapper reports it.
async fn execute(
    mut stream: UnixStream,
    request_id: u64,
    workspace: String,
    wrapper: Identity,
    manager: Arc<Manager>,
    kind: ExecutionKind,
) -> Result<()> {
    let (plan, mut stop) = match manager
        .begin_execution(&workspace, Some(wrapper), kind)
        .await
    {
        Ok(begun) => begun,
        Err(error) => {
            let body = Body::error("execution_failed", format!("{error:#}"));
            return protocol::write(&mut stream, &Response::new(request_id, body)).await;
        }
    };
    let execution_id = plan.id.clone();
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
    if result.is_ok() {
        protocol::write(&mut writer, &Control::Finished { complete }).await?;
    }
    result.map(|_| ())
}
