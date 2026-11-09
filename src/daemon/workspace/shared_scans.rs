//! Process scans that advisory checks share. A check joins the next scan to
//! start, which covers the executions of every check waiting for it, so
//! checks made at the same time inventory the processes once rather than once
//! each, and none reads a scan that started before it asked. Destructive
//! operations verify ownership with their own scans instead.
use crate::client::execution_ids::ExecutionId;
use std::{
    collections::HashSet,
    future::Future,
    sync::{Arc, Mutex, PoisonError},
};

use anyhow::{Context, Result, anyhow};
use tokio::sync::watch;

use crate::process::identity::{self as process, Scan};

#[cfg(test)]
mod tests;

type Outcome = Result<Arc<Scan>, Arc<anyhow::Error>>;

#[derive(Default)]
pub(crate) struct SharedScans {
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    /// The scan that starts next, gathering the executions to cover.
    next: Option<Batch>,
    /// Whether a task runs scans, so a check that joins the next one only
    /// starts a task when none does.
    running: bool,
}

struct Batch {
    ids: HashSet<ExecutionId>,
    done: watch::Sender<Option<Outcome>>,
}

impl SharedScans {
    /// Live same-user processes carrying one of the execution markers
    /// `ids`, among others, from a scan that started after this call.
    pub(crate) async fn scan(&self, ids: HashSet<ExecutionId>) -> Result<Arc<Scan>> {
        self.scan_with(ids, process::scan).await
    }

    /// [`Self::scan`] that runs a scan it starts with `scan`.
    async fn scan_with<F, Fut>(&self, ids: HashSet<ExecutionId>, scan: F) -> Result<Arc<Scan>>
    where
        F: Fn(HashSet<ExecutionId>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Scan>> + Send + 'static,
    {
        if ids.is_empty() {
            return Ok(Arc::default());
        }
        let mut done = self.join(ids, scan);
        let outcome = done
            .wait_for(Option::is_some)
            .await
            .context("shared process scan ended without a result")?
            .clone()
            .context("shared process scan ended without a result")?;
        outcome.map_err(|error| anyhow!("{error:#}"))
    }

    /// Add `ids` to the next scan and start a task to run it unless one runs.
    fn join<F, Fut>(&self, ids: HashSet<ExecutionId>, scan: F) -> watch::Receiver<Option<Outcome>>
    where
        F: Fn(HashSet<ExecutionId>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Scan>> + Send + 'static,
    {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let batch = state.next.get_or_insert_with(|| Batch {
            ids: HashSet::new(),
            done: watch::Sender::new(None),
        });
        batch.ids.extend(ids);
        let done = batch.done.subscribe();
        if !state.running {
            state.running = true;
            tokio::spawn(run(self.state.clone(), scan));
        }
        done
    }
}

/// Run the next scan until no check waits for one. Running apart from the
/// checks, a scan completes for the others when the check that started it
/// stops waiting.
async fn run<F, Fut>(state: Arc<Mutex<State>>, scan: F)
where
    F: Fn(HashSet<ExecutionId>) -> Fut,
    Fut: Future<Output = Result<Scan>>,
{
    let _runner = Runner(state.clone());
    while let Some(batch) = take_next(&state) {
        let outcome = scan(batch.ids).await.map(Arc::new).map_err(Arc::new);
        batch.done.send_replace(Some(outcome));
    }
}

/// The batch to scan next, or none, which leaves no task running.
fn take_next(state: &Mutex<State>) -> Option<Batch> {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    let batch = state.next.take();
    state.running = batch.is_some();
    batch
}

/// The task running scans. A panic ends it, failing the checks waiting for
/// the next scan, and the next check starts another.
struct Runner(Arc<Mutex<State>>);

impl Drop for Runner {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            state.next = None;
            state.running = false;
        }
    }
}
