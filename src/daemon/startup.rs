//! Startup work the daemon does after binding its socket. Meanwhile requests
//! wait for the daemon to finish starting, as across an update's handoff,
//! except that a request whose client sends it again when closed unanswered is
//! closed, and a status request that would outwait its client learns which
//! step runs and for how long. A step that runs long is logged.
use std::sync::{Arc, Mutex};

use tokio::{
    net::{UnixListener, UnixStream},
    sync::watch,
    task::{JoinHandle, JoinSet},
    time::Instant,
};

use super::{Server, connections, log, read_request, serve_request};
use crate::protocol::{self, Body, DaemonMethod, ErrorCode, Method, Operation, Response, timing};

/// The startup in progress and the task accepting its connections.
pub(super) struct Startup {
    step: Arc<Mutex<Step>>,
    /// The server once startup has finished, which waiting requests use.
    ready: watch::Sender<Option<Server>>,
    accepting: JoinHandle<(UnixListener, JoinSet<()>)>,
}

#[derive(Clone, Copy)]
struct Step {
    name: &'static str,
    since: Instant,
}

impl Startup {
    /// Accept connections on `listener` until [`Self::finish`].
    pub(super) fn begin(listener: UnixListener) -> Self {
        let step = Arc::new(Mutex::new(Step {
            name: "starting",
            since: Instant::now(),
        }));
        let (ready, server) = watch::channel(None);
        let accepting = tokio::spawn(accept(listener, step.clone(), server));
        Self {
            step,
            ready,
            accepting,
        }
    }

    /// Run the step `name`, such as `auditing worktrees`, which status
    /// requests report while it runs.
    pub(super) async fn step<T>(&self, name: &'static str, work: impl Future<Output = T>) -> T {
        *lock(&self.step) = Step {
            name,
            since: Instant::now(),
        };
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
    step: Arc<Mutex<Step>>,
    server: watch::Receiver<Option<Server>>,
) -> (UnixListener, JoinSet<()>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = ready(server.clone()) => break,
            accepted = connections::accept(&listener) => match accepted {
                Ok(stream) => {
                    connections.spawn(connection(stream, step.clone(), server.clone()));
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
    step: Arc<Mutex<Step>>,
    server: watch::Receiver<Option<Server>>,
) {
    let Ok(Some(request)) = read_request(&mut stream).await else {
        return;
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
            Err(_) => return answer_step(stream, request.id, &step).await,
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

/// Refuse a status request with the startup step running and how long it has run.
async fn answer_step(mut stream: UnixStream, request_id: u64, step: &Mutex<Step>) {
    let Step { name, since } = *lock(step);
    let message = format!(
        "the daemon is starting: {name} for {}",
        log::elapsed(since.elapsed())
    );
    let response = Response::new(request_id, Body::error(ErrorCode::OperationFailed, message));
    let _ = protocol::write(&mut stream, &response).await;
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
