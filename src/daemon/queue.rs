//! First-come order for queued acquisitions; each allocator keeps its own
//! capacity rules and consults the queue only before claiming capacity.
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::Duration,
};
use tokio::time::Instant;

use crate::daemon::{allocation::Allocation, approvals::Target};

/// A waiter that has not polled for this long has stopped waiting. Requests in
/// progress never expire, so a long simulator boot keeps every place.
const ABANDONED_AFTER: Duration = Duration::from_secs(30);

/// Queued lease names per target, earliest first. Kept in memory: waiting
/// commands poll, so they rejoin after a daemon restart.
#[derive(Default)]
pub(crate) struct Queues(Mutex<HashMap<Target, VecDeque<Waiter>>>);

struct Waiter {
    workspace_id: String,
    name: String,
    seen: Instant,
    in_progress: usize,
}

/// One acquisition's view of the queue. Dropping it leaves the queue unless
/// [`Self::settle`] saw a busy outcome.
pub(crate) struct Place<'a> {
    queues: &'a Queues,
    target: Target,
    workspace_id: String,
    name: String,
    /// Whether this request counts toward a waiter's requests in progress.
    held: bool,
    keep: bool,
}

impl Queues {
    /// A request outside the queue keeps an existing place but does not take one.
    pub(crate) fn enter(
        &self,
        target: Target,
        workspace_id: &str,
        name: &str,
        join: bool,
    ) -> Place<'_> {
        let now = Instant::now();
        let mut queues = self.0.lock().expect("queues lock");
        let queue = queues.entry(target.clone()).or_default();
        queue.retain(|w| w.in_progress > 0 || now.duration_since(w.seen) < ABANDONED_AFTER);
        let held = if let Some(waiter) = queue.iter_mut().find(|w| w.is(workspace_id, name)) {
            waiter.seen = now;
            waiter.in_progress += 1;
            true
        } else if join {
            queue.push_back(Waiter {
                workspace_id: workspace_id.into(),
                name: name.into(),
                seen: now,
                in_progress: 1,
            });
            true
        } else {
            false
        };
        if queue.is_empty() {
            queues.remove(&target);
        }
        Place {
            queues: self,
            target,
            workspace_id: workspace_id.into(),
            name: name.into(),
            held,
            keep: false,
        }
    }
}

impl Waiter {
    fn is(&self, workspace_id: &str, name: &str) -> bool {
        self.workspace_id == workspace_id && self.name == name
    }
}

impl Place<'_> {
    /// Waiters queued before this request; a request outside the queue is
    /// behind all of them.
    pub(crate) fn ahead(&self) -> usize {
        let queues = self.queues.0.lock().expect("queues lock");
        let Some(queue) = queues.get(&self.target) else {
            return 0;
        };
        queue
            .iter()
            .position(|w| w.is(&self.workspace_id, &self.name))
            .unwrap_or(queue.len())
    }

    /// Busy outcomes keep the place; grants, approvals and errors give it up.
    pub(crate) fn settle<T, E>(&mut self, outcome: &Result<Allocation<T>, E>) {
        self.keep = matches!(outcome, Ok(Allocation::Busy(_)));
    }
}

/// The busy outcome a request gets while earlier waiters remain.
pub(crate) fn behind<T>(ahead: usize, subject: &str) -> Option<Allocation<T>> {
    match ahead {
        0 => None,
        1 => Some(Allocation::Busy(format!(
            "1 earlier request is queued for {subject}"
        ))),
        ahead => Some(Allocation::Busy(format!(
            "{ahead} earlier requests are queued for {subject}"
        ))),
    }
}

impl Drop for Place<'_> {
    fn drop(&mut self) {
        if !self.held {
            return;
        }
        let mut queues = self.queues.0.lock().expect("queues lock");
        let Some(queue) = queues.get_mut(&self.target) else {
            return;
        };
        if let Some(index) = queue
            .iter()
            .position(|w| w.is(&self.workspace_id, &self.name))
        {
            if self.keep {
                let waiter = &mut queue[index];
                waiter.seen = Instant::now();
                waiter.in_progress = waiter.in_progress.saturating_sub(1);
            } else {
                queue.remove(index);
            }
        }
        if queue.is_empty() {
            queues.remove(&self.target);
        }
    }
}

#[cfg(test)]
mod tests;
