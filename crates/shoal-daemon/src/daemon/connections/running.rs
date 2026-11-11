//! The requests running now, so the daemon can say what holds its request
//! capacity.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use tokio::time::Instant;

use crate::daemon::log::elapsed;

#[derive(Default)]
pub(super) struct Running {
    requests: Mutex<Requests>,
}

#[derive(Default)]
struct Requests {
    next: u64,
    started: BTreeMap<u64, (String, Instant)>,
}

/// A running request's record, removed when it is dropped.
pub(super) struct Entry {
    running: Arc<Running>,
    id: u64,
}

impl Running {
    pub(super) fn start(self: &Arc<Self>, method: String) -> Entry {
        let mut requests = self.lock();
        let id = requests.next;
        requests.next += 1;
        requests.started.insert(id, (method, Instant::now()));
        Entry {
            running: self.clone(),
            id,
        }
    }

    /// The running requests by method, most first, each with how long the
    /// oldest has run, such as `120 agent_updates (oldest 2h 3m)`.
    pub(super) fn summary(&self) -> String {
        let now = Instant::now();
        let mut methods = BTreeMap::<&str, (usize, Instant)>::new();
        let requests = self.lock();
        for (method, started) in requests.started.values() {
            let (count, oldest) = methods.entry(method).or_insert((0, *started));
            *count += 1;
            *oldest = (*oldest).min(*started);
        }
        let mut methods = methods.into_iter().collect::<Vec<_>>();
        methods.sort_by_key(|(_, (count, oldest))| (std::cmp::Reverse(*count), *oldest));
        methods
            .into_iter()
            .map(|(method, (count, oldest))| {
                format!("{count} {method} (oldest {})", elapsed(now - oldest))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Requests> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.running.lock().started.remove(&self.id);
    }
}

#[cfg(test)]
mod tests;
