//! What the daemon keeps in memory about the agent queue: who holds starting
//! entries, which stopped agents wait for their restore, which one-shot runs
//! wait for or hold their admission, what held back waiting entries at the last
//! admission pass, and what wakes the dispatcher.
use crate::client::execution_ids::ExecutionId;
use crate::client::repository_ids::RepositoryId;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, MutexGuard, Notify, futures::Notified, watch};

use super::Grant;
use crate::client::{queue::QueueEntryId, space_ids::SpaceId, time::UnixTime};
use crate::model::HeldBy;

#[derive(Default)]
pub(crate) struct AgentQueue {
    admission: Mutex<()>,
    /// Open holds per starting queue entry.
    holds: StdMutex<HashMap<QueueEntryId, usize>>,
    /// Executions whose stopped agent waits for its queued restore with
    /// every process stopped, with what admitted it once admission did.
    restore_waiters: StdMutex<HashMap<ExecutionId, Option<Grant>>>,
    /// One-shot runs by request number, so the oldest comes first.
    oneshots: StdMutex<OneShots>,
    /// What held back each waiting entry at the last admission pass.
    held_back: StdMutex<HashMap<QueueEntryId, HeldBy>>,
    /// Since when each entry has been held back for its own memory, pass
    /// after pass.
    memory_since: StdMutex<HashMap<QueueEntryId, Instant>>,
    wake: Notify,
    /// Admission waits while the system shuts down.
    suspended: AtomicBool,
}

#[derive(Default)]
struct OneShots {
    next: u64,
    runs: BTreeMap<u64, OneShot>,
}

/// A one-shot run's request for admission in `repository`, for the work it
/// serves; it holds no agent slot, but its memory counts.
struct OneShot {
    repository: RepositoryId,
    serving: Option<Serving>,
    requested_at: UnixTime,
    /// Whether protection stopped its earlier run, so it waits for healthy
    /// readings as a stopped agent's restore does.
    after_stop: bool,
    /// When admission let it start, which it holds until it ends.
    admitted: watch::Sender<Option<Instant>>,
    /// Stops the run for protection, until it is used or the run cannot be
    /// stopped.
    stop: Option<Box<dyn FnOnce() + Send + Sync>>,
}

/// Which admitted one-shot runs protection stops.
pub(crate) enum OneShotStop {
    /// The run that started last.
    Newest,
    All,
}

/// A one-shot request waiting for admission.
pub(crate) struct WaitingOneShot {
    /// Its queue ID, which no stored entry has.
    pub id: QueueEntryId,
    pub repository: RepositoryId,
    /// What it serves, which it ranks with.
    pub serving: Option<Serving>,
    pub requested_at: UnixTime,
    pub after_stop: bool,
}

/// The work a one-shot run serves, which places it in the queue.
#[derive(Clone)]
pub(crate) struct Serving {
    /// The space whose work it serves.
    pub space: SpaceId,
    /// The sequence number of the newest queue entry when it asked, so it
    /// arrives after that entry and before any queued later.
    pub queued_after: i64,
}

/// The queue ID of one-shot request `number`, which no stored entry has.
fn oneshot_id(number: u64) -> QueueEntryId {
    format!("{ONESHOT_PREFIX}{number}").into()
}

const ONESHOT_PREFIX: &str = "oneshot:";

impl AgentQueue {
    /// Serialize admission, so two passes cannot fill one slot.
    pub(crate) async fn lock_admission(&self) -> MutexGuard<'_, ()> {
        self.admission.lock().await
    }

    /// Count an open hold on the entry `id`.
    pub(crate) fn hold(&self, id: &QueueEntryId) {
        *self.holds.lock().unwrap().entry(id.clone()).or_default() += 1;
    }

    /// Drop a hold on `id`.
    pub(crate) fn release(&self, id: &QueueEntryId) {
        let mut holds = self.holds.lock().unwrap();
        if let Some(count) = holds.get_mut(id) {
            *count -= 1;
            if *count == 0 {
                holds.remove(id);
            }
        }
    }

    /// The entries with an open hold.
    pub(crate) fn held(&self) -> HashSet<QueueEntryId> {
        self.holds.lock().unwrap().keys().cloned().collect()
    }

    /// Replace what held back waiting entries with the pass's `held` at
    /// `now`, and keep since when each one held for its memory has been,
    /// which an entry held for anything else or started starts over.
    pub(crate) fn record_held_back(&self, held: HashMap<QueueEntryId, HeldBy>, now: Instant) {
        let mut since = self.memory_since.lock().unwrap();
        since.retain(|id, _| matches!(held.get(id), Some(HeldBy::Memory { .. })));
        for (id, held) in &held {
            if let HeldBy::Memory { .. } = held {
                since.entry(id.clone()).or_insert(now);
            }
        }
        *self.held_back.lock().unwrap() = held;
    }

    /// How long, at `now`, the entry `id` has been held back for its memory.
    pub(crate) fn waited_for_memory(&self, id: &QueueEntryId, now: Instant) -> Duration {
        let since = self.memory_since.lock().unwrap();
        since
            .get(id)
            .map_or(Duration::ZERO, |since| now.duration_since(*since))
    }

    /// What held back the entry `id` at the last admission pass.
    pub(crate) fn held_back(&self, id: &QueueEntryId) -> Option<HeldBy> {
        self.held_back.lock().unwrap().get(id).copied()
    }

    /// Mark the stopped agent of `execution` as waiting for its restore
    /// while the returned waiter lives.
    pub(crate) fn wait_for_restore(&self, execution: &ExecutionId) -> RestoreWaiter<'_> {
        self.restore_waiters
            .lock()
            .unwrap()
            .insert(execution.to_owned(), None);
        RestoreWaiter {
            queue: self,
            execution: execution.to_owned(),
        }
    }

    /// The executions whose agent waits for its restore.
    pub(crate) fn restore_waiters(&self) -> HashSet<ExecutionId> {
        self.restore_waiters
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    /// Record that `grant` admitted the restore of `execution`, if its agent
    /// still waits.
    pub(crate) fn grant_restore(&self, execution: &ExecutionId, grant: Grant) {
        if let Some(granted) = self.restore_waiters.lock().unwrap().get_mut(execution) {
            *granted = Some(grant);
        }
    }

    /// What admitted the restore of `execution`, if admission did.
    pub(crate) fn restore_grant(&self, execution: &ExecutionId) -> Option<Grant> {
        self.restore_waiters
            .lock()
            .unwrap()
            .get(execution)
            .copied()
            .flatten()
    }

    /// Ask for admission in `repository` for a one-shot run, such as a
    /// review, of the work it is `serving`, which waits in the queue at the
    /// rank of that space; the request and the admission end with the
    /// returned value.
    /// Protection calls `stop` to stop the run; a run it stopped asks again
    /// `after_stop`.
    pub(crate) fn request_oneshot(
        &self,
        repository: &RepositoryId,
        serving: Serving,
        after_stop: bool,
        stop: impl FnOnce() + Send + Sync + 'static,
    ) -> OneShotAdmission<'_> {
        self.request(repository, Some(serving), after_stop, stop)
    }

    fn request(
        &self,
        repository: &RepositoryId,
        serving: Option<Serving>,
        after_stop: bool,
        stop: impl FnOnce() + Send + Sync + 'static,
    ) -> OneShotAdmission<'_> {
        let (admitted, waiting) = watch::channel(None);
        let mut oneshots = self.oneshots.lock().unwrap();
        let number = oneshots.next;
        oneshots.next += 1;
        oneshots.runs.insert(
            number,
            OneShot {
                repository: repository.clone(),
                serving,
                requested_at: UnixTime::now(),
                after_stop,
                admitted,
                stop: Some(Box::new(stop)),
            },
        );
        drop(oneshots);
        self.wake();
        OneShotAdmission {
            queue: self,
            number,
            admitted: waiting,
        }
    }

    /// Hold an admission in `repository` until the daemon restarts, for a run a
    /// stopped daemon left whose processes could not be proven stopped.
    pub(crate) fn hold_oneshot(&self, repository: &RepositoryId) {
        let admission = self.request(repository, None, false, || {});
        if let Some(run) = self.oneshots.lock().unwrap().runs.get(&admission.number) {
            run.admitted.send_replace(Some(Instant::now()));
        }
        admission.keep();
    }

    /// The one-shot requests still waiting, oldest first.
    pub(crate) fn waiting_oneshots(&self) -> Vec<WaitingOneShot> {
        self.oneshots
            .lock()
            .unwrap()
            .runs
            .iter()
            .filter(|(_, run)| run.admitted.borrow().is_none())
            .map(|(number, run)| WaitingOneShot {
                id: oneshot_id(*number),
                repository: run.repository.clone(),
                serving: run.serving.clone(),
                requested_at: run.requested_at,
                after_stop: run.after_stop,
            })
            .collect()
    }

    /// Stop the admitted one-shot runs `which` picks among those not yet
    /// stopped and not held, returning how many.
    pub(crate) fn stop_oneshots(&self, which: OneShotStop) -> usize {
        let mut oneshots = self.oneshots.lock().unwrap();
        let mut stoppable: Vec<&mut OneShot> = oneshots
            .runs
            .values_mut()
            .filter(|run| run.admitted.borrow().is_some() && run.stop.is_some())
            .collect();
        if let OneShotStop::Newest = which {
            stoppable.sort_by_key(|run| *run.admitted.borrow());
            stoppable.drain(..stoppable.len().saturating_sub(1));
        }
        for run in &mut stoppable {
            if let Some(stop) = run.stop.take() {
                stop();
            }
        }
        stoppable.len()
    }

    /// The repository of each admitted one-shot run, with how long it has
    /// run.
    pub(crate) fn admitted_oneshots(&self) -> Vec<(RepositoryId, std::time::Duration)> {
        self.oneshots
            .lock()
            .unwrap()
            .runs
            .values()
            .filter_map(|run| {
                Some((
                    run.repository.clone(),
                    run.admitted.borrow().as_ref()?.elapsed(),
                ))
            })
            .collect()
    }

    /// Admit the one-shot request with queue ID `id`; false when
    /// `id` names no one-shot request.
    pub(crate) fn admit_oneshot(&self, id: &QueueEntryId) -> bool {
        let Some(number) = id
            .as_str()
            .strip_prefix(ONESHOT_PREFIX)
            .and_then(|number| number.parse().ok())
        else {
            return false;
        };
        if let Some(run) = self.oneshots.lock().unwrap().runs.get(&number) {
            run.admitted.send_replace(Some(Instant::now()));
        }
        true
    }

    /// Hold back admission while the system shuts down, so nothing the
    /// shutdown stopped starts again before it; resuming wakes the dispatcher.
    /// Takes the admission gate, so an admission pass already past its
    /// check finishes first.
    pub(crate) async fn suspend(&self, suspended: bool) {
        let _gate = self.lock_admission().await;
        self.suspended.store(suspended, Ordering::SeqCst);
        if !suspended {
            self.wake();
        }
    }

    pub(crate) fn suspended(&self) -> bool {
        self.suspended.load(Ordering::SeqCst)
    }

    /// Wake the dispatcher, since a slot may have freed or work may fit.
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Resolves once something wakes the dispatcher.
    pub(crate) fn woken(&self) -> Notified<'_> {
        self.wake.notified()
    }
}

/// A one-shot run's place in the queue, then its admission, while it lives.
pub(crate) struct OneShotAdmission<'a> {
    queue: &'a AgentQueue,
    number: u64,
    admitted: watch::Receiver<Option<Instant>>,
}

impl OneShotAdmission<'_> {
    /// Wait until admission lets the run start.
    pub(crate) async fn admitted(&mut self) {
        // The sender lives as long as this request.
        let _ = self.admitted.wait_for(Option::is_some).await;
    }

    /// Keep the admission of a run whose processes may still be running until the
    /// daemon restarts. Protection no longer picks it, as nothing stops it.
    pub(crate) fn keep(self) {
        if let Some(run) = self
            .queue
            .oneshots
            .lock()
            .unwrap()
            .runs
            .get_mut(&self.number)
        {
            run.stop = None;
        }
        std::mem::forget(self);
    }
}

impl Drop for OneShotAdmission<'_> {
    fn drop(&mut self) {
        self.queue
            .oneshots
            .lock()
            .unwrap()
            .runs
            .remove(&self.number);
        // A freed slot may let another entry start.
        self.queue.wake();
    }
}

/// Marks a restore's agent as waiting for admission while it lives.
pub(crate) struct RestoreWaiter<'a> {
    queue: &'a AgentQueue,
    execution: ExecutionId,
}

impl Drop for RestoreWaiter<'_> {
    fn drop(&mut self) {
        self.queue
            .restore_waiters
            .lock()
            .unwrap()
            .remove(&self.execution);
    }
}
