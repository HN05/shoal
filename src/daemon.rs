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
    protocol::{self, Body, Method, Request, Response, Status},
    workspace::Manager,
};

// Never unlink the lock file: waiters must all lock the same inode.
struct Ownership {
    paths: Paths,
    _lock: File,
}
impl Drop for Ownership {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.paths.socket);
    }
}

pub async fn run(paths: Paths, managed: bool) -> Result<()> {
    paths.prepare()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(paths.state.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("a daemon already owns this state directory")?;
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
    let started = Instant::now();
    let mut clients = JoinSet::new();
    eprintln!("shoal daemon listening on {}", paths.socket.display());
    let cleanup = manager.config.auto_cleanup.enabled.then(|| {
        tokio::spawn(crate::cleanup::run(
            manager.clone(),
            Duration::from_secs(manager.config.auto_cleanup.idle_minutes * 60),
        ))
    });
    let sim_cleanup = {
        let manager = manager.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(15)).await;
                if let Err(error) = manager.expire_simulators().await {
                    eprintln!("simulator cleanup: {error:#}");
                }
            }
        })
    };
    loop {
        tokio::select! {
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
            _ = shutdown_rx.changed() => break,
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            accepted = listener.accept(), if clients.len() < 128 => {
                let (stream, _) = accepted?;
                let shutdown = shutdown.clone();
                let manager = manager.clone();
                clients.spawn(async move {
                    if let Err(error) = serve(stream, started, managed, shutdown, manager).await {
                        eprintln!("client connection: {error:#}");
                    }
                });
            }
        }
    }
    sim_cleanup.abort();
    let _ = sim_cleanup.await;
    clients.abort_all();
    if let Some(cleanup) = cleanup {
        cleanup.abort();
        let _ = cleanup.await;
    }
    while clients.join_next().await.is_some() {}
    Ok(())
}

async fn serve(
    mut stream: UnixStream,
    started: Instant,
    managed: bool,
    shutdown: watch::Sender<bool>,
    manager: Arc<Manager>,
) -> Result<()> {
    let mut request: Request =
        match timeout(Duration::from_secs(5), protocol::read(&mut stream)).await? {
            Ok(request) => request,
            Err(error) => {
                return protocol::write(
                    &mut stream,
                    &Response {
                        protocol: protocol::VERSION,
                        id: 0,
                        body: Body::Error {
                            code: "invalid_request".into(),
                            message: error.to_string(),
                        },
                    },
                )
                .await;
            }
        };
    let scope = if request.protocol == protocol::VERSION {
        match crate::scope::authorize(&manager, request.scope.as_deref(), &mut request.method).await
        {
            Ok(scope) => scope,
            Err(error) => {
                return protocol::write(
                    &mut stream,
                    &Response {
                        protocol: protocol::VERSION,
                        id: request.id,
                        body: Body::Error {
                            code: "scope_denied".into(),
                            message: format!("{error:#}"),
                        },
                    },
                )
                .await;
            }
        }
    } else {
        None
    };
    let execution_id = match request.scope.as_deref() {
        Some(token) => manager
            .scopes
            .lock()
            .await
            .get(token)
            .map(|(execution, _)| execution.clone()),
        None => None,
    };
    if request.protocol == protocol::VERSION {
        if let Method::Execute { workspace, wrapper } = request.method {
            return execute(stream, request.id, workspace, wrapper, manager).await;
        }
    }
    let mut stop = false;
    let body = if request.protocol != protocol::VERSION {
        Body::Error {
            code: "protocol_mismatch".into(),
            message: "restart the daemon with the installed version".into(),
        }
    } else {
        match request.method {
            Method::Status => Body::Status(Status {
                pid: std::process::id(),
                version: env!("CARGO_PKG_VERSION").into(),
                uptime_secs: started.elapsed().as_secs(),
                managed,
            }),
            Method::Shutdown if managed => Body::Error {
                code: "managed_service".into(),
                message: "stop the daemon through the OS service manager".into(),
            },
            Method::Shutdown => {
                stop = true;
                Body::Ok
            }
            method => match operation(&manager, method, scope.as_deref(), execution_id).await {
                Ok(body) => body,
                Err(error) => Body::Error {
                    code: "operation_failed".into(),
                    message: format!("{error:#}"),
                },
            },
        }
    };
    protocol::write(
        &mut stream,
        &Response {
            protocol: protocol::VERSION,
            id: request.id,
            body,
        },
    )
    .await?;
    if stop {
        let _ = shutdown.send(true);
    }
    Ok(())
}

async fn operation(
    manager: &Manager,
    method: Method,
    scope: Option<&str>,
    execution_id: Option<String>,
) -> Result<Body> {
    Ok(match method {
        Method::ResourceAcquire { workspace, request } => {
            match manager.acquire_resource(workspace, request).await? {
                crate::resources::Acquisition::Acquired(lease) => Body::ResourceLease(lease),
                crate::resources::Acquisition::Busy(message) => Body::ResourceBusy { message },
            }
        }
        Method::ResourceRelease {
            workspace,
            pool,
            name,
        } => {
            manager.release_resource(workspace, pool, name).await?;
            Body::Ok
        }
        Method::ResourceList { workspace } => {
            Body::ResourceLeases(manager.list_resources(workspace).await?)
        }
        Method::ResourceOverview { workspace } => {
            Body::ResourceOverview(manager.resource_overview(workspace).await?)
        }
        Method::SimHistory {
            workspace,
            limit,
            before,
        } => {
            let owner = match workspace {
                Some(selector) => Some(manager.get(selector).await?.id),
                None => None,
            };
            Body::SimHistory(manager.clean_history(owner, limit, before).await?)
        }
        Method::SimCatalog => {
            let inventory = crate::simctl::inventory().await?;
            Body::SimCatalog(
                serde_json::json!({"device_types":inventory.devicetypes, "runtimes":inventory.runtimes.into_iter().filter(|r| r.is_available).collect::<Vec<_>>(), "policy":manager.config.simulators}),
            )
        }
        Method::SimList { workspace } => {
            let owner = match workspace {
                Some(selector) => Some(manager.get(selector).await?.id),
                None => None,
            };
            Body::Simulators(manager.simulators(owner).await?)
        }
        Method::SimAcquire { workspace, request } => {
            match manager
                .acquire_simulator(workspace, request, execution_id)
                .await?
            {
                crate::simulators::Acquisition::Acquired(sim) => Body::Simulator(*sim),
                crate::simulators::Acquisition::Busy(message) => Body::SimBusy { message },
            }
        }
        Method::SimRelease { workspace, name } => {
            manager.release_simulator(workspace, name).await?;
            Body::Ok
        }
        Method::Repositories => {
            let mut repos = manager.repositories().await?;
            if let Some(scope) = scope {
                let owner = manager.get(scope.into()).await?;
                repos.retain(|r| r.id == owner.repository_id);
            }
            Body::Repositories(repos)
        }
        Method::Register { source, name, path } => {
            Body::Repository(manager.register(source, name, path).await?)
        }
        Method::RenameRepository { repository, name } => {
            Body::Repository(manager.rename_repository(repository, name).await?)
        }
        Method::RemoveRepository { repository } => {
            Body::RepositoryRemoved(manager.remove_repository(repository).await?)
        }
        Method::Add {
            repository,
            name,
            base,
        } => Body::Workspace(manager.add(repository, name, base).await?),
        Method::List => {
            let mut workspaces = manager.list().await?;
            if let Some(scope) = scope {
                workspaces.retain(|w| w.id == scope);
            }
            Body::Workspaces(workspaces)
        }
        Method::PullMain { workspace } => Body::PulledMain(manager.pull_main(workspace).await?),
        Method::Reconcile { workspace, options } => {
            let workspaces = match workspace {
                Some(workspace) => vec![manager.get(workspace).await?],
                None => manager.list().await?,
            };
            let mut reports = Vec::new();
            for workspace in workspaces {
                let report = match manager.reconcile(workspace.id.clone(), options).await {
                    Ok(report) => report,
                    Err(error) => crate::recovery::Report {
                        workspace,
                        directory: crate::recovery::DirectoryState::Unverified,
                        moved_to: None,
                        executions: vec![],
                        changes: vec![],
                        issues: vec![format!("{error:#}")],
                    },
                };
                reports.push(report);
            }
            Body::Reconciliation(reports)
        }
        Method::DiffBase { workspace } => Body::DiffBase(manager.diff_base(workspace).await?),
        Method::ReservePort {
            workspace,
            name,
            port,
            env_var,
            reason,
            on_conflict,
        } => match manager
            .reserve_port(
                workspace,
                name,
                crate::ports::PortOptions {
                    requested: port,
                    env_var,
                    reason,
                    on_conflict,
                },
            )
            .await?
        {
            crate::ports::ReserveOutcome::Reserved(port) => Body::Port(port),
            crate::ports::ReserveOutcome::Suggested(proposal) => Body::PortSuggestion(proposal),
        },
        Method::PortOverview { workspace } => {
            Body::PortOverview(manager.port_overview(workspace).await?)
        }
        Method::Ports { workspace } => Body::Ports(manager.list_ports(workspace).await?),
        Method::ReleasePort { workspace, name } => {
            manager.release_port(workspace, name).await?;
            Body::Ok
        }
        Method::Inspect { workspace } => Body::Inspection(manager.inspect(workspace).await?),
        Method::CheckRemoval {
            workspace,
            caller_pid,
        } => Body::RemovalCheck(manager.check_removal(workspace, caller_pid).await?),
        Method::Remove {
            workspace,
            choice,
            caller_pid,
        } => Body::RemovalResult(manager.remove(workspace, choice, caller_pid).await?),
        Method::Stop { workspace } => {
            manager.stop(workspace).await?;
            Body::Ok
        }
        _ => anyhow::bail!("unsupported operation"),
    })
}

async fn execute(
    mut stream: UnixStream,
    request_id: u64,
    workspace: String,
    wrapper: crate::process_identity::Identity,
    manager: Arc<Manager>,
) -> Result<()> {
    let (plan, mut stop) = match manager.begin(workspace, Some(wrapper)).await {
        Ok(result) => result,
        Err(error) => {
            return protocol::write(
                &mut stream,
                &Response {
                    protocol: protocol::VERSION,
                    id: request_id,
                    body: Body::Error {
                        code: "execution_failed".into(),
                        message: format!("{error:#}"),
                    },
                },
            )
            .await;
        }
    };
    let execution_id = plan.id.clone();
    let (mut reader, mut writer) = stream.into_split();
    let result = async {
        protocol::write(
            &mut writer,
            &Response {
                protocol: protocol::VERSION,
                id: request_id,
                body: Body::Execution(plan),
            },
        )
        .await?;
        match protocol::read::<protocol::ExecutionEvent>(&mut reader).await? {
            protocol::ExecutionEvent::Finished { .. } => return Ok(()),
            protocol::ExecutionEvent::Started { child, group_id } => {
                manager
                    .record_execution_child(execution_id.clone(), child, group_id)
                    .await?;
                protocol::write(&mut writer, &protocol::Control::Started).await?;
            }
        }
        let finished = protocol::read::<protocol::ExecutionEvent>(&mut reader);
        tokio::pin!(finished);
        let mut sent_stop = false;
        loop {
            tokio::select! {
                result = &mut finished => break match result? {
                    protocol::ExecutionEvent::Finished { .. } => Ok(()),
                    _ => anyhow::bail!("unexpected execution event"),
                },
                changed = stop.changed(), if !sent_stop => {
                    changed?;
                    protocol::write(&mut writer, &protocol::Control::Stop).await?;
                    sent_stop = true;
                }
            }
        }
    }
    .await;
    let complete = manager.finish(execution_id, result.is_ok()).await?;
    if result.is_ok() {
        protocol::write(&mut writer, &protocol::Control::Finished { complete }).await?;
    }
    result.map(|_| ())
}
