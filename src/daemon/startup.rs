//! Startup work the daemon does after binding its socket. Meanwhile a status
//! request learns which step runs and for how long, a request whose client
//! sends it again when closed unanswered is closed, and other requests wait
//! for the daemon to finish starting. A step that runs long is logged.
use std::sync::{Arc, Mutex};

use tokio::{
    net::{UnixListener, UnixStream},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
    time::Instant,
};

use super::{connections, log, read_request};
use crate::protocol::{self, Body, DaemonMethod, ErrorCode, Method, Operation, Request, Response};

/// A connection whose request waits for the daemon to finish starting.
pub(super) type Held = (UnixStream, Request);

/// The startup in progress and the task answering its connections.
pub(super) struct Startup {
    step: Arc<Mutex<Step>>,
    finish: Option<oneshot::Sender<()>>,
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
        let (finish, finished) = oneshot::channel();
        let answering = tokio::spawn(answer(listener, step.clone(), finished));
        Self {
            step,
            finish: Some(finish),
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
        if let Some(finish) = self.finish.take() {
            let _ = finish.send(());
        }
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
    mut finished: oneshot::Receiver<()>,
) -> (UnixListener, Vec<Held>) {
    let mut reading = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut finished => break,
            accepted = connections::accept(&listener) => match accepted {
                Ok(stream) => {
                    reading.spawn(hold_or_answer(stream, step.clone()));
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

/// The connection when its request waits for startup, after answering a
/// status request with the running step or closing a request its client
/// sends again.
async fn hold_or_answer(mut stream: UnixStream, step: Arc<Mutex<Step>>) -> Option<Held> {
    let request = read_request(&mut stream).await.ok().flatten()?;
    if request.method.retries_when_closed() {
        return None;
    }
    if !matches!(
        request.method,
        Method::Operation(Operation::Daemon(DaemonMethod::Status))
    ) {
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

fn lock(step: &Mutex<Step>) -> std::sync::MutexGuard<'_, Step> {
    step.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
