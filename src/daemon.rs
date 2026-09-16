use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
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
    let listener = UnixListener::bind(&paths.socket).context("bind daemon socket")?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let started = Instant::now();
    let mut clients = JoinSet::new();
    eprintln!("shoal daemon listening on {}", paths.socket.display());
    loop {
        tokio::select! {
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
            _ = shutdown_rx.changed() => break,
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            accepted = listener.accept(), if clients.len() < 128 => {
                let (stream, _) = accepted?;
                let shutdown = shutdown.clone();
                clients.spawn(async move {
                    let _ = timeout(Duration::from_secs(5), serve(stream, started, managed, shutdown)).await;
                });
            }
        }
    }
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    Ok(())
}

async fn serve(
    mut stream: UnixStream,
    started: Instant,
    managed: bool,
    shutdown: watch::Sender<bool>,
) -> Result<()> {
    let request: Request = match protocol::read(&mut stream).await {
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
