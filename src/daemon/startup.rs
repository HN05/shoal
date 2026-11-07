//! Startup work the daemon does after binding its socket. Meanwhile requests
//! wait for the daemon to finish starting, as across an update's handoff,
//! except that a request whose client sends it again when closed unanswered is
//! closed, and a status request that would outwait its client learns which
//! step runs, for how long, and the file access it waits on. A step that runs
//! long is logged.
use std::{
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use tokio::{
    net::{UnixListener, UnixStream},
    sync::watch,
    task::{JoinHandle, JoinSet},
    time::Instant,
};

use super::{Server, connections, file_access::FileAccess, log, read_request, serve_request};
use crate::protocol::{self, Body, DaemonMethod, ErrorCode, Method, Operation, Response, timing};

/// A check of a path that has run this long is named in a status answer.
const SLOW_ACCESS: Duration = Duration::from_secs(5);

/// The startup in progress and the task accepting its connections.
pub(super) struct Startup {
    progress: Arc<Progress>,
    /// The server once startup has finished, which waiting requests use.
    ready: watch::Sender<Option<Server>>,
    accepting: JoinHandle<(UnixListener, JoinSet<()>)>,
}

/// What a status request learns while startup runs.
struct Progress {
    step: Mutex<Step>,
    /// The file access the steps wait on, once the state is open.
    access: OnceLock<Arc<FileAccess>>,
}

#[derive(Clone, Copy)]
struct Step {
    name: &'static str,
    since: Instant,
}

impl Startup {
    /// Accept connections on `listener` until [`Self::finish`].
    pub(super) fn begin(listener: UnixListener) -> Self {
        let progress = Arc::new(Progress {
            step: Mutex::new(Step {
                name: "starting",
                since: Instant::now(),
            }),
            access: OnceLock::new(),
        });
        let (ready, server) = watch::channel(None);
        let accepting = tokio::spawn(accept(listener, progress.clone(), server));
        Self {
            progress,
            ready,
            accepting,
        }
    }

    /// Name the checks of `access` that wait long in status answers.
    pub(super) fn follow(&self, access: Arc<FileAccess>) {
        let _ = self.progress.access.set(access);
    }

    /// Run the step `name`, such as `auditing worktrees`, which status
    /// requests report while it runs.
    pub(super) async fn step<T>(&self, name: &'static str, work: impl Future<Output = T>) -> T {
        *lock(&self.progress.step) = Step {
            name,
            since: Instant::now(),
        };
        let _span = crate::profile::span("startup", || name.to_owned());
        log::when_slow(&format!("startup step {name}"), work).await
    }

    /// Serve the waiting requests with `server`, and return the listener with
    /// the connections accepted so far, which go on without holding it up.
    pub(super) async fn finish(mut self, server: Server) -> (UnixListener, JoinSet<()>) {
        self.ready.send_replace(Some(server));
        (&mut self.accepting)
            .await
            .expect("the startup accept task does not panic")
    }
}

impl Drop for Startup {
    /// A startup that failed stops accepting, which drops its connections.
    fn drop(&mut self) {
        self.accepting.abort();
    }
}

async fn accept(
    listener: UnixListener,
    progress: Arc<Progress>,
    server: watch::Receiver<Option<Server>>,
) -> (UnixListener, JoinSet<()>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = ready(server.clone()) => break,
            accepted = connections::accept(&listener) => match accepted {
                Ok(stream) => {
                    connections.spawn(connection(stream, progress.clone(), server.clone()));
                }
                Err(error) => {
                    log!("accept connection during startup: {error}");
                    break;
                }
            },
        }
    }
    (listener, connections)
}

/// Serve a connection's request once startup has finished, after closing a
/// request its client sends again, or answering a status request with the
/// running step once startup outlasts [`timing::STARTUP_STATUS_WAIT`].
async fn connection(
    mut stream: UnixStream,
    progress: Arc<Progress>,
    server: watch::Receiver<Option<Server>>,
) {
    let request = match read_request(&mut stream).await {
        Ok(Some(request)) => request,
        Ok(None) | Err(_) => return,
    };
    if request.method.retries_when_closed() {
        return;
    }
    let status = matches!(
        request.method,
        Method::Operation(Operation::Daemon(DaemonMethod::Status))
    );
    let server = if status {
        match tokio::time::timeout(timing::STARTUP_STATUS_WAIT, ready(server)).await {
            Ok(server) => server,
            Err(_) => return answer_step(stream, request.id, &progress).await,
        }
    } else {
        ready(server).await
    };
    // A startup that failed sends no server.
    let Some(server) = server else {
        return;
    };
    let operation = server.manager.gates.owned_operation().await;
    if let Err(error) = serve_request(stream, server, request, operation).await {
        log!("client connection: {error:#}");
    }
}

/// Refuse a status request with the startup step running, how long it has
/// run, and the file access it waits on.
async fn answer_step(mut stream: UnixStream, request_id: u64, progress: &Progress) {
    let Step { name, since } = *lock(&progress.step);
    let mut message = format!(
        "the daemon is starting: {name} for {}",
        log::elapsed(since.elapsed())
    );
    if let Some(waiting) = progress.access.get().and_then(|access| waiting(access)) {
        message.push_str(&format!("; {waiting}"));
    }
    let response = Response::new(request_id, Body::error(ErrorCode::OperationFailed, message));
    let _ = protocol::write(&mut stream, &response).await;
}

/// The stalled directory, or else the check that has run longer than
/// [`SLOW_ACCESS`], with the permission it may wait on.
fn waiting(access: &FileAccess) -> Option<String> {
    let waiting = match access.stalls().first() {
        Some(stall) => format!("file access in {} stalled", stall.directory.display()),
        None => {
            let (path, waited) = access
                .waiting()
                .filter(|(_, waited)| *waited >= SLOW_ACCESS)?;
            format!(
                "waiting {} for file access in {}",
                log::elapsed(waited),
                path.display()
            )
        }
    };
    Some(format!(
        "{waiting}; a macOS privacy prompt may be waiting for an answer"
    ))
}

/// The server once startup has finished, or `None` once it failed.
async fn ready(mut server: watch::Receiver<Option<Server>>) -> Option<Server> {
    let ready = server.wait_for(Option::is_some).await.ok()?;
    ready.clone()
}

fn lock(step: &Mutex<Step>) -> std::sync::MutexGuard<'_, Step> {
    step.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
