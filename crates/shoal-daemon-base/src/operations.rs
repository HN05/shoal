//! Operations: work the daemon starts in a worker process of its own, so it
//! continues when its caller goes, and whose progress callers follow by its
//! ID. The worker reports its steps and its end; what its processes write
//! arrives on its standard streams. An ended operation can be followed for
//! [`RETAIN`].
mod worker;

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::scope_tokens::ScopeToken;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use anyhow::Result;
use shoal_protocol::paths::Paths;
use tokio::sync::watch;
use uuid::Uuid;

use crate::callers::Caller;
use crate::gates::Gates;
use crate::protocol::operations::{
    Ended, Failure, OperationEvent, OperationKind, OperationSpec, Order, Progress, Step, Stream,
};
#[cfg(any(test, feature = "test-support"))]
pub use worker::fake;

/// How long an ended operation can still be followed.
const RETAIN: Duration = Duration::from_secs(600);
/// The most events an operation keeps; older output goes first.
const KEPT_EVENTS: usize = 10_000;

pub struct Operations {
    paths: Paths,
    gates: Arc<Gates>,
    entries: Mutex<HashMap<String, Arc<Entry>>>,
    /// Scope token → the caller a running worker acts as, so the worker
    /// keeps its caller's scope, execution included, after that caller
    /// ends, until the worker exits.
    scopes: Mutex<HashMap<ScopeToken, Caller>>,
    /// Whether workers are fakes tests drive.
    #[cfg(any(test, feature = "test-support"))]
    faked: std::sync::atomic::AtomicBool,
}

/// One operation's progress, as its followers read it.
pub struct Entry {
    /// The ID of the workspace it works in.
    pub workspace: SpaceId,
    pub kind: OperationKind,
    /// The space of the scoped caller that started it, whose processes
    /// follow it whatever became of the space it works in.
    starter: Option<SpaceId>,
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
    pub fn new(paths: Paths, gates: Arc<Gates>) -> Self {
        Self {
            paths,
            gates,
            entries: Mutex::default(),
            scopes: Mutex::default(),
            #[cfg(any(test, feature = "test-support"))]
            faked: Default::default(),
        }
    }

    /// Start fake workers that tests drive from now on.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fake_workers(&self) {
        self.faked.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the daemon starts fake workers that tests drive, of
    /// operations and of the other work it runs detached; only a test
    /// daemon does.
    pub fn fakes(&self) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        return self.faked.load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(any(test, feature = "test-support")))]
        false
    }

    /// Start `spec`, which works in the space `workspace`, in a worker that
    /// acts with `caller`'s scope, in `env` when given, returning the
    /// operation's ID.
    pub async fn spawn(
        self: &Arc<Self>,
        workspace: SpaceId,
        spec: OperationSpec,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&Caller>,
    ) -> Result<String> {
        let token = caller.map(|caller| self.issue(caller));
        let starter = caller.map(|caller| caller.space_id.clone());
        let (id, entry) = self.register(workspace, spec.kind(), starter);
        let order = Order {
            id: id.clone(),
            spec,
        };
        let mut worker = match self.start_worker(&order, env, token.as_ref()) {
            Ok(worker) => worker,
            Err(error) => {
                if let Some(token) = &token {
                    self.revoke(token);
                }
                self.forget(&id);
                return Err(error);
            }
        };
        let interrupt = worker.interrupt.take();
        entry.update(|state| state.interrupt = interrupt);
        // An update waits for the operation, which its worker cannot report
        // to the replacement.
        let operation = self.gates.owned_operation().await;
        let operations = self.clone();
        let released = async move {
            if let Some(token) = token {
                operations.revoke(&token);
            }
        };
        let operations = self.clone();
        let ended = id.clone();
        tokio::spawn(async move {
            worker::supervise(entry, worker, operation, released).await;
            operations.expire(&ended).await;
        });
        Ok(id)
    }

    /// Start the worker of `order`, a fake one where tests drive them.
    fn start_worker(
        &self,
        order: &Order,
        env: Option<BTreeMap<String, String>>,
        token: Option<&ScopeToken>,
    ) -> Result<worker::Worker> {
        #[cfg(any(test, feature = "test-support"))]
        if self.fakes() {
            return Ok(fake::start(order, token));
        }
        worker::spawn(&self.paths, order, env, token)
    }

    /// Whether an operation in the workspace `id` still runs.
    pub fn runs_in(&self, id: &SpaceId) -> bool {
        (self.running().iter()).any(|entry| &entry.workspace == id)
    }

    /// Whether operation `id` is over: it ended, or this machine does not
    /// know it.
    pub fn over(&self, id: &str) -> bool {
        (self.get(id)).is_none_or(|entry| entry.events_from(u64::MAX).1.is_some())
    }

    /// How operation `id` ended, or how its worker reported it ends, once
    /// either came; none for one this machine does not know.
    pub fn ending_of(&self, id: &str) -> Option<Option<Ended>> {
        self.get(id).map(|entry| entry.ending())
    }

    /// How operation `id` ends, once it does; none for one this machine
    /// does not know.
    pub async fn ended(&self, id: &str) -> Option<Ended> {
        let entry = self.get(id)?;
        let mut changed = entry.watch();
        loop {
            if let (_, Some(ended)) = entry.events_from(u64::MAX) {
                return Some(ended);
            }
            changed.changed().await.ok()?;
        }
    }

    /// Record a new operation of `kind` in `workspace`, started by a
    /// process of the space `starter`, or by the user.
    fn register(
        &self,
        workspace: SpaceId,
        kind: OperationKind,
        starter: Option<SpaceId>,
    ) -> (String, Arc<Entry>) {
        let id = Uuid::new_v4().to_string();
        let entry = Arc::new(Entry {
            workspace,
            kind,
            starter,
            state: Default::default(),
            changed: watch::Sender::new(()),
        });
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.insert(id.clone(), entry.clone());
        (id, entry)
    }

    /// The operation `id`, while it runs or can still be followed.
    pub fn get(&self, id: &str) -> Option<Arc<Entry>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.get(id).cloned()
    }

    /// Whether operation `id` still runs.
    pub fn runs(&self, id: &str) -> bool {
        self.get(id).is_some_and(|entry| entry.running())
    }

    /// The workspaces an operation works in until its worker ends.
    pub fn working_in(&self) -> HashSet<SpaceId> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        (entries.values())
            .filter(|entry| entry.running())
            .map(|entry| entry.workspace.clone())
            .collect()
    }

    /// The operations whose worker has yet to end them.
    pub fn running(&self) -> Vec<Arc<Entry>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .values()
            .filter(|entry| entry.running())
            .cloned()
            .collect()
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

    /// A new token that acts as `caller` for a worker, an operation's or
    /// another the daemon runs for the caller, until [`Self::revoke`]
    /// revokes it.
    pub fn issue(&self, caller: &Caller) -> ScopeToken {
        let token = ScopeToken::generate();
        let mut scopes = self.scopes.lock().unwrap_or_else(PoisonError::into_inner);
        scopes.insert(token.clone(), caller.clone());
        token
    }

    pub fn revoke(&self, token: &ScopeToken) {
        let mut scopes = self.scopes.lock().unwrap_or_else(PoisonError::into_inner);
        scopes.remove(token);
    }

    /// The caller a worker's `token` acts as, while the worker runs.
    pub fn caller(&self, token: &ScopeToken) -> Option<Caller> {
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

    /// Whether a process of `caller`'s space started it.
    pub fn started_by(&self, caller: &Caller) -> bool {
        self.starter.as_ref() == Some(&caller.space_id)
    }

    /// Record the step the worker reported.
    pub fn report_step(&self, step: Step) {
        self.push(Progress::Step { step });
    }

    /// Keep the end the worker reported, which takes effect once it exits.
    pub fn report_end(&self, ended: Ended) {
        self.update(|state| state.reported = Some(ended));
    }

    /// Interrupt the worker, as an interrupt of the command that ran its
    /// work did; nothing once it exited.
    pub fn interrupt(&self) {
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

    /// How it ended, or how its worker reported it ends, once either came.
    pub fn ending(&self) -> Option<Ended> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.ended.clone().or_else(|| state.reported.clone())
    }

    /// Whether its worker has yet to end it.
    pub fn running(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.ended.is_none()
    }

    /// The kept events from the `after`th on, and the end once it came.
    pub fn events_from(&self, after: u64) -> (Vec<OperationEvent>, Option<Ended>) {
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
    pub fn watch(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }
}

#[cfg(test)]
mod tests;
