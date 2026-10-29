//! What the daemon keeps in memory about the agent queue: who holds starting
//! entries, which stopped agents wait for their restore, and what wakes the
//! dispatcher.
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::sync::{Mutex, MutexGuard, Notify, futures::Notified};

use super::Grant;

#[derive(Default)]
pub(crate) struct AgentQueue {
    admission: Mutex<()>,
    /// Open holds per starting queue entry.
    holds: StdMutex<HashMap<String, usize>>,
    /// Executions whose stopped agent waits for its queued restore with
    /// every process stopped, with what admitted it once admission did.
    restore_waiters: StdMutex<HashMap<String, Option<Grant>>>,
    wake: Notify,
    /// Admission waits while the system shuts down.
    suspended: AtomicBool,
}

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
