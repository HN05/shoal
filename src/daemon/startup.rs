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

use super::{connections, log, read_request};
use crate::protocol::{
    self, Body, DaemonMethod, ErrorCode, Method, Operation, Request, Response, timing,
};

/// A connection whose request waits for the daemon to finish starting.
pub(super) type Held = (UnixStream, Request);

/// The startup in progress and the task answering its connections.
pub(super) struct Startup {
    step: Arc<Mutex<Step>>,
    finished: watch::Sender<bool>,
    answering: JoinHandle<(UnixListener, Vec<Held>)>,
}

#[derive(Clone, Copy)]
struct Step {
    name: &'static str,
    since: Instant,
}

impl Startup {
    /// Answer connections on `listener` until [`Self::finish`].
    pub(super) fn begin(listener: UnixListener) -> Self {
        let step = Arc::new(Mutex::new(Step {
            name: "starting",
            since: Instant::now(),
        }));
        let (finished, watching) = watch::channel(false);
        let answering = tokio::spawn(answer(listener, step.clone(), watching));
        Self {
            step,
            finished,
            answering,
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

    /// The listener back, with the connections whose requests waited.
    pub(super) async fn finish(mut self) -> (UnixListener, Vec<Held>) {
        self.finished.send_replace(true);
        (&mut self.answering)
            .await
            .expect("the startup connection task does not panic")
    }
}

impl Drop for Startup {
    /// A startup that failed stops answering.
    fn drop(&mut self) {
        self.answering.abort();
    }
}

async fn answer(
    listener: UnixListener,
    step: Arc<Mutex<Step>>,
    finished: watch::Receiver<bool>,
) -> (UnixListener, Vec<Held>) {
    let mut reading = JoinSet::new();
    loop {
        tokio::select! {
            _ = until_finished(finished.clone()) => break,
            accepted = connections::accept(&listener) => match accepted {
                Ok(stream) => {
                    reading.spawn(hold_or_answer(stream, step.clone(), finished.clone()));
                }
                Err(error) => {
                    log!("accept connection during startup: {error}");
                    break;
                }
            },
        }
    }
    let held = reading.join_all().await.into_iter().flatten().collect();
    (listener, held)
}

/// The connection when its request waits for startup, after closing a
/// request its client sends again, or answering a status request with the
/// running step once startup outlasts [`timing::STARTUP_STATUS_WAIT`].
async fn hold_or_answer(
    mut stream: UnixStream,
    step: Arc<Mutex<Step>>,
    finished: watch::Receiver<bool>,
) -> Option<Held> {
    let request = read_request(&mut stream).await.ok().flatten()?;
    if request.method.retries_when_closed() {
        return None;
    }
    let status = matches!(
        request.method,
        Method::Operation(Operation::Daemon(DaemonMethod::Status))
    );
    let waited = tokio::time::timeout(timing::STARTUP_STATUS_WAIT, until_finished(finished));
    if !status || waited.await.is_ok() {
        return Some((stream, request));
    }
    let Step { name, since } = *lock(&step);
    let message = format!(
        "the daemon is starting: {name} for {}",
        log::elapsed(since.elapsed())
    );
    let response = Response::new(request.id, Body::error(ErrorCode::OperationFailed, message));
    let _ = protocol::write(&mut stream, &response).await;
    None
}

async fn until_finished(mut finished: watch::Receiver<bool>) {
    // The sender lives as long as the startup that answers connections.
    let _ = finished.wait_for(|finished| *finished).await;
}

fn lock(step: &Mutex<Step>) -> std::sync::MutexGuard<'_, Step> {
    step.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
