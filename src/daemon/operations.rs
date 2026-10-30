//! Operations: work the daemon starts in a worker process of its own, so it
//! continues when its caller goes, and whose progress callers follow by its
//! ID. The worker reports its steps and its end; what its processes write
//! arrives on its standard streams. An ended operation can be followed for
//! [`RETAIN`].
mod worker;

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use anyhow::{Result, ensure};
use tokio::sync::watch;
use uuid::Uuid;

use super::{
    refusal::{Reason, Refusal},
    scope::Caller,
    workspace::Manager,
};
use crate::protocol::operations::{
    Ended, Failure, OperationEvent, OperationSpec, Order, Progress, Report, Stream,
};
#[cfg(test)]
pub(crate) use worker::fake;

/// How long an ended operation can still be followed.
const RETAIN: Duration = Duration::from_secs(600);
/// The most events an operation keeps; older output goes first.
const KEPT_EVENTS: usize = 10_000;
/// The longest output line an event holds, so every event fits a frame.
const LINE_BYTES: usize = 8 * 1024;

#[derive(Default)]
pub(crate) struct Operations {
    entries: Mutex<HashMap<String, Arc<Entry>>>,
}

/// One operation's progress, as its followers read it.
pub(crate) struct Entry {
    /// The ID of the workspace it works in.
    pub workspace: String,
    state: Mutex<State>,
    changed: watch::Sender<()>,
}

#[derive(Default)]
struct State {
    events: VecDeque<OperationEvent>,
    next: u64,
    /// Interrupts the worker, until it exits.
    interrupt: Option<worker::Interrupt>,
    /// The end the worker reported, which takes effect once it exits.
    reported: Option<Ended>,
    ended: Option<(Ended, Instant)>,
}

impl Operations {
    /// Record a new operation in `workspace`.
    fn register(&self, workspace: String) -> (String, Arc<Entry>) {
        let id = Uuid::new_v4().to_string();
        let entry = Arc::new(Entry {
            workspace,
            state: Default::default(),
            changed: watch::Sender::new(()),
        });
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, entry| !entry.expired());
        entries.insert(id.clone(), entry.clone());
        (id, entry)
    }

    /// The operation `id`, while it runs or can still be followed.
    fn get(&self, id: &str) -> Option<Arc<Entry>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.get(id).filter(|entry| !entry.expired()).cloned()
    }
}

impl Entry {
    fn update(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner));
        self.changed.send_replace(());
    }

    fn push(&self, progress: Progress) {
        self.update(|state| {
            if state.events.len() == KEPT_EVENTS {
                state.events.pop_front();
            }
            state.events.push_back(OperationEvent {
                index: state.next,
                progress,
            });
            state.next += 1;
        });
    }

    /// Record a line the worker's processes wrote to `stream`.
    fn output(&self, stream: Stream, line: &[u8]) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let mut line = String::from_utf8_lossy(line).into_owned();
        if line.len() > LINE_BYTES {
            line.truncate(line.floor_char_boundary(LINE_BYTES));
            line.push('…');
        }
        self.push(Progress::Output { stream, line });
    }

    /// Interrupt the worker, as an interrupt of the command that ran its
    /// work did; nothing once it exited.
    pub(crate) fn interrupt(&self) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(interrupt) = &state.interrupt {
            interrupt();
        }
    }

    /// End the operation as its worker reported, or, when it exited with
    /// `code` without a report, as failed.
    fn end(&self, code: i32) {
        self.update(|state| {
            let ended = state.reported.take().unwrap_or_else(|| {
                Ended::Failed(Failure::Failed {
                    message: format!("the operation's worker exited with status {code}"),
                })
            });
            state.ended = Some((ended, Instant::now()));
            state.interrupt = None;
        });
    }

    fn expired(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state
            .ended
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= RETAIN)
    }

    /// The kept events from the `after`th on, and the end once it came.
    pub(crate) fn since(&self, after: u64) -> (Vec<OperationEvent>, Option<Ended>) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let events = state
            .events
            .iter()
            .filter(|event| event.index >= after)
            .cloned()
            .collect();
        (events, state.ended.as_ref().map(|(ended, _)| ended.clone()))
    }

    /// Notified on each change.
    pub(crate) fn watch(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }
}

impl Manager {
    /// Start `spec` in a worker that acts with `caller`'s scope, in `env`
    /// when given, returning the operation's ID. The scope check already
    /// narrowed a scoped caller's workspace to an ID.
    pub(crate) async fn start_operation(
        &self,
        mut spec: OperationSpec,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&Caller>,
    ) -> Result<String> {
        let workspace = self.workspace(spec.workspace_mut()).await?.id;
        *spec.workspace_mut() = workspace.clone();
        let token = match caller {
            Some(caller) => Some(self.scope_token(caller).await?),
            None => None,
        };
        let (id, entry) = self.operations.register(workspace);
        let order = Order {
            id: id.clone(),
            spec,
        };
        let mut worker = match worker::spawn(&self.paths, &order, env, token.as_deref()) {
            Ok(worker) => worker,
            Err(error) => {
                entry.end(1);
                return Err(error);
            }
        };
        let interrupt = worker.interrupt.take();
        entry.update(|state| state.interrupt = interrupt);
        tokio::spawn(worker::supervise(entry, worker));
        Ok(id)
    }

    /// Operation `id`, refusing a scoped caller one outside its tree.
    pub(crate) async fn operation(&self, id: &str, caller: Option<&Caller>) -> Result<Arc<Entry>> {
        let entry = self.operations.get(id).ok_or_else(|| {
            Refusal::new(Reason::NotFound, format!("operation {id} does not exist"))
        })?;
        if let Some(caller) = caller {
            ensure!(
                self.within_subtree(&caller.workspace_id, &entry.workspace)
                    .await?,
                "workspace processes can only follow operations of their own workspace and those split from it"
            );
        }
        Ok(entry)
    }

    /// Record the worker's `report` of operation `id`.
    pub(crate) async fn report_operation(
        &self,
        id: &str,
        report: Report,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let entry = self.operation(id, caller).await?;
        match report {
            Report::Step(step) => entry.push(Progress::Step { step }),
            Report::Ended(ended) => entry.update(|state| state.reported = Some(ended)),
        }
        Ok(())
    }

    /// A token that grants `caller`'s scope: its execution's while it is
    /// connected, or its workspace's.
    async fn scope_token(&self, caller: &Caller) -> Result<String> {
        if let Some(execution) = caller.execution_id()
            && let Some(token) = self.executions.token_of(execution).await
        {
            return Ok(token);
        }
        self.workspace_scope_token(&caller.workspace_id).await
    }
}

#[cfg(test)]
mod tests;
