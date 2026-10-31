//! What the daemon keeps in memory about the agent queue: who holds starting
//! entries, which stopped agents wait for their restore, which one-shot runs
//! wait for or hold a slot, and what wakes the dispatcher.
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use tokio::sync::{Mutex, MutexGuard, Notify, futures::Notified, watch};

use super::Grant;

#[derive(Default)]
pub(crate) struct AgentQueue {
    admission: Mutex<()>,
    /// Open holds per starting queue entry.
    holds: StdMutex<HashMap<String, usize>>,
    /// Executions whose stopped agent waits for its queued restore with
    /// every process stopped, with what admitted it once admission did.
    restore_waiters: StdMutex<HashMap<String, Option<Grant>>>,
    /// One-shot runs by request number, so the oldest comes first.
    oneshots: StdMutex<OneShots>,
    wake: Notify,
    /// Admission waits while the system shuts down.
    suspended: AtomicBool,
}

#[derive(Default)]
struct OneShots {
    next: u64,
    runs: BTreeMap<u64, OneShot>,
}

/// A one-shot run's request for a slot in `repository`.
struct OneShot {
    repository: String,
    /// When admission gave it a slot, which it holds until it ends.
    admitted: watch::Sender<Option<Instant>>,
}

/// The queue ID of one-shot request `number`, which no stored entry has.
fn oneshot_id(number: u64) -> String {
    format!("{ONESHOT_PREFIX}{number}")
}

const ONESHOT_PREFIX: &str = "oneshot:";

impl AgentQueue {
    /// Serialize admission, so two passes cannot fill one slot.
    pub(crate) async fn lock_admission(&self) -> MutexGuard<'_, ()> {
        self.admission.lock().await
    }

    /// Count an open hold on the starting entry `id`.
    pub(crate) fn hold(&self, id: &str) {
        *self.holds.lock().unwrap().entry(id.to_owned()).or_default() += 1;
    }

    /// Drop a hold on `id`.
    pub(crate) fn release(&self, id: &str) {
        let mut holds = self.holds.lock().unwrap();
        if let Some(count) = holds.get_mut(id) {
            *count -= 1;
            if *count == 0 {
                holds.remove(id);
            }
        }
    }

    /// The starting entries with an open hold.
    pub(crate) fn held(&self) -> HashSet<String> {
        self.holds.lock().unwrap().keys().cloned().collect()
    }

    /// Mark the stopped agent of `execution` as waiting for its restore
    /// while the returned waiter lives.
    pub(crate) fn wait_for_restore(&self, execution: &str) -> RestoreWaiter<'_> {
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
    pub(crate) fn restore_waiters(&self) -> HashSet<String> {
        self.restore_waiters
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    /// Record that `grant` admitted the restore of `execution`, if its agent
    /// still waits.
    pub(crate) fn grant_restore(&self, execution: &str, grant: Grant) {
        if let Some(granted) = self.restore_waiters.lock().unwrap().get_mut(execution) {
            *granted = Some(grant);
        }
    }

    /// What admitted the restore of `execution`, if admission did.
    pub(crate) fn restore_grant(&self, execution: &str) -> Option<Grant> {
        self.restore_waiters
            .lock()
            .unwrap()
            .get(execution)
            .copied()
            .flatten()
    }

    /// Ask for a slot in `repository` for a one-shot run, such as a review,
    /// which waits in the queue ahead of launches; the request and the slot
    /// end with the returned value.
    pub(crate) fn request_oneshot(&self, repository: &str) -> OneShotSlot<'_> {
        let (admitted, waiting) = watch::channel(None);
        let mut oneshots = self.oneshots.lock().unwrap();
        let number = oneshots.next;
        oneshots.next += 1;
        oneshots.runs.insert(
            number,
            OneShot {
                repository: repository.to_owned(),
                admitted,
            },
        );
        drop(oneshots);
        self.wake();
        OneShotSlot {
            queue: self,
            number,
            admitted: waiting,
        }
    }

    /// The one-shot requests still waiting, oldest first, by queue ID with
    /// their repository.
    pub(crate) fn waiting_oneshots(&self) -> Vec<(String, String)> {
        self.oneshots
            .lock()
            .unwrap()
            .runs
            .iter()
            .filter(|(_, run)| run.admitted.borrow().is_none())
            .map(|(number, run)| (oneshot_id(*number), run.repository.clone()))
            .collect()
    }

    /// The repository of each one-shot run holding a slot, with how long it
    /// has held it.
    pub(crate) fn admitted_oneshots(&self) -> Vec<(String, std::time::Duration)> {
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

    /// Give the one-shot request with queue ID `id` its slot; false when
    /// `id` names no one-shot request.
    pub(crate) fn admit_oneshot(&self, id: &str) -> bool {
        let Some(number) = id
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

/// A one-shot run's place in the queue, then its slot, while it lives.
pub(crate) struct OneShotSlot<'a> {
    queue: &'a AgentQueue,
    number: u64,
    admitted: watch::Receiver<Option<Instant>>,
}

impl OneShotSlot<'_> {
    /// Wait until admission gives the run its slot.
    pub(crate) async fn admitted(&mut self) {
        // The sender lives as long as this request.
        let _ = self.admitted.wait_for(Option::is_some).await;
    }
}

impl Drop for OneShotSlot<'_> {
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
    execution: String,
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
