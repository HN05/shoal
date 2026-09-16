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
    let config = crate::config::Config::load(&paths)?;
    let listener = UnixListener::bind(&paths.socket).context("bind daemon socket")?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let started = Instant::now();
    let mut clients = JoinSet::new();
    eprintln!("shoal daemon listening on {}", paths.socket.display());
    let cleanup = config.auto_cleanup.enabled.then(|| {
        tokio::spawn(crate::cleanup::run(
            manager.clone(),
            Duration::from_secs(config.auto_cleanup.idle_minutes * 60),
        ))
    });
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
    let request: Request =
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
    if request.protocol == protocol::VERSION {
        if let Method::Execute { workspace } = request.method {
            return execute(stream, request.id, workspace, manager).await;
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
            method => match operation(&manager, method).await {
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

async fn operation(manager: &Manager, method: Method) -> Result<Body> {
    Ok(match method {
        Method::Repositories => Body::Repositories(manager.repositories().await?),
        Method::Register { source } => Body::Repository(manager.register(source).await?),
        Method::Add {
            repository,
            name,
            base,
        } => Body::Workspace(manager.add(repository, name, base).await?),
        Method::List => Body::Workspaces(manager.list().await?),
        Method::Inspect { workspace } => Body::Inspection(manager.inspect(workspace).await?),
        Method::CheckRemoval {
            workspace,
            caller_pid,
        } => Body::RemovalCheck(manager.check_removal(workspace, caller_pid).await?),
        Method::Remove {
            workspace,
            confirmed,
            caller_pid,
        } => {
            manager.remove(workspace, confirmed, caller_pid).await?;
            Body::Ok
        }
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
    manager: Arc<Manager>,
) -> Result<()> {
    let (plan, mut stop) = match manager.begin(workspace).await {
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
        let finished = protocol::read::<protocol::ExecutionResult>(&mut reader);
        tokio::pin!(finished);
        let mut sent_stop = false;
        loop {
            tokio::select! {
                result = &mut finished => break result,
                changed = stop.changed(), if !sent_stop => {
                    changed?;
                    protocol::write(&mut writer, &protocol::Control::Stop).await?;
                    sent_stop = true;
                }
            }
        }
    }
    .await;
    manager.finish(execution_id, result.is_ok()).await?;
    if result.is_ok() {
        protocol::write(&mut writer, &protocol::Control::Finished).await?;
    }
    result.map(|_| ())
}
