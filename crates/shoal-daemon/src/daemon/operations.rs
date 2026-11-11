//! Operations: work the daemon starts in a worker process of its own, so it
//! continues when its caller goes, and whose progress callers follow by its
//! ID. The worker reports its steps and its end; what its processes write
//! arrives on its standard streams. An ended operation can be followed for
//! [`RETAIN`].
pub(crate) mod starts;
mod worker;

use crate::client::scope_tokens::ScopeToken;
use crate::client::space_ids::SpaceId;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use anyhow::{Result, ensure};
use tokio::sync::watch;
use uuid::Uuid;

use super::{scope::Caller, workspace::Manager};
use crate::client::space_names::SpaceSelector;
use crate::protocol::operations::{
    Ended, Failure, OperationEvent, OperationSpec, Order, Progress, Report, Stream,
};
use crate::refusal::{Reason, Refusal};
#[cfg(test)]
pub(crate) use worker::fake;

/// How long an ended operation can still be followed.
const RETAIN: Duration = Duration::from_secs(600);
/// The most events an operation keeps; older output goes first.
const KEPT_EVENTS: usize = 10_000;

#[derive(Default)]
pub(crate) struct Operations {
    entries: Mutex<HashMap<String, Arc<Entry>>>,
    /// Scope token → the caller a running worker acts as, so the worker
    /// keeps its caller's scope, execution included, after that caller
    /// ends, until the worker exits.
    scopes: Mutex<HashMap<ScopeToken, Caller>>,
}

/// One operation's progress, as its followers read it.
pub(crate) struct Entry {
    /// The ID of the workspace it works in.
    pub workspace: SpaceId,
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
    ended: Option<Ended>,
}

impl Operations {
    /// Record a new operation in `workspace`.
    fn register(&self, workspace: SpaceId) -> (String, Arc<Entry>) {
        let id = Uuid::new_v4().to_string();
        let entry = Arc::new(Entry {
            workspace,
            state: Default::default(),
            changed: watch::Sender::new(()),
        });
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.insert(id.clone(), entry.clone());
        (id, entry)
    }

    /// The operation `id`, while it runs or can still be followed.
    fn get(&self, id: &str) -> Option<Arc<Entry>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.get(id).cloned()
    }

    /// Forget operation `id`; followers that hold it keep reading it.
    fn forget(&self, id: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.remove(id);
    }

    /// Forget operation `id`, which just ended, once [`RETAIN`] has passed.
    async fn expire(&self, id: &str) {
        tokio::time::sleep(RETAIN).await;
        self.forget(id);
    }

    /// A new token that acts as `caller` until [`Self::revoke`] revokes it.
    fn issue(&self, caller: &Caller) -> ScopeToken {
        let token = ScopeToken::generate();
        let mut scopes = self.scopes.lock().unwrap_or_else(PoisonError::into_inner);
        scopes.insert(token.clone(), caller.clone());
        token
    }

    fn revoke(&self, token: &ScopeToken) {
        let mut scopes = self.scopes.lock().unwrap_or_else(PoisonError::into_inner);
        scopes.remove(token);
    }

    /// The caller a worker's `token` acts as, while the worker runs.
    pub(crate) fn caller(&self, token: &ScopeToken) -> Option<Caller> {
        let scopes = self.scopes.lock().unwrap_or_else(PoisonError::into_inner);
        scopes.get(token).cloned()
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
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let line = String::from_utf8_lossy(line).into_owned();
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
            state.ended = Some(ended);
            state.interrupt = None;
        });
    }

    /// Whether its worker has yet to end it.
    pub(crate) fn running(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.ended.is_none()
    }

    /// The kept events from the `after`th on, and the end once it came.
    pub(crate) fn events_from(&self, after: u64) -> (Vec<OperationEvent>, Option<Ended>) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let events = state
            .events
            .iter()
            .filter(|event| event.index >= after)
            .cloned()
            .collect();
        (events, state.ended.clone())
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
        let workspace = match spec {
            OperationSpec::Unmerge { .. } => self.unmerge_workspace(spec.workspace_mut()).await?,
            _ => self.workspace(spec.workspace_mut()).await?,
        }
        .id;
        *spec.workspace_mut() = SpaceSelector::id(&workspace);
        let token = caller.map(|caller| self.operations.issue(caller));
        let (id, entry) = self.operations.register(workspace.clone());
        let order = Order {
            id: id.clone(),
            spec,
        };
        let mut worker = match worker::spawn(&self.paths, &order, env, token.as_ref()) {
            Ok(worker) => worker,
            Err(error) => {
                if let Some(token) = &token {
                    self.operations.revoke(token);
                }
                self.operations.forget(&id);
                return Err(error);
            }
        };
        let interrupt = worker.interrupt.take();
        entry.update(|state| state.interrupt = interrupt);
        // An update waits for the operation, which its worker cannot report
        // to the replacement.
        let operation = self.gates.owned_operation().await;
        let operations = self.operations.clone();
        let released = async move {
            if let Some(token) = token {
                operations.revoke(&token);
            }
        };
        let operations = self.operations.clone();
        let ended = id.clone();
        tokio::spawn(async move {
            worker::supervise(entry, worker, operation, released).await;
            operations.expire(&ended).await;
        });
        Ok(id)
    }

    /// Whether operation `id` still runs.
    pub(crate) fn operation_running(&self, id: &str) -> bool {
        self.operations.get(id).is_some_and(|entry| entry.running())
    }

    /// Operation `id`, refusing a scoped caller one outside its tree.
    pub(crate) async fn operation(&self, id: &str, caller: Option<&Caller>) -> Result<Arc<Entry>> {
        let entry = self.operations.get(id).ok_or_else(|| {
            Refusal::new(Reason::NotFound, format!("operation {id} does not exist"))
        })?;
        if let Some(caller) = caller {
            ensure!(
                self.reaches(caller, &entry.workspace).await?,
                "workspace processes can only follow operations of their own space and those below it"
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
            Report::Ended(ended) => {
                self.keep_preparation_end(&entry.workspace, id, &ended)
                    .await;
                entry.update(|state| state.reported = Some(ended));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
