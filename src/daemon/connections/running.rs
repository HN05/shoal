//! The requests running now, so the daemon can say what holds its request
//! capacity and log requests that run longer than expected.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::time::Instant;

use crate::daemon::log;

/// A request running this long is logged, once, and again when it ends.
pub(super) const SLOW_REQUEST: Duration = Duration::from_secs(60);

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

/// Run `operation`, logging it once it has run for [`SLOW_REQUEST`] and
/// again when it ends, so a request that hangs leaves a trace.
pub(in crate::daemon) async fn logged_when_slow<T>(
    method: &str,
    operation: impl Future<Output = T>,
) -> T {
    let started = Instant::now();
    let mut operation = std::pin::pin!(operation);
    tokio::select! {
        output = &mut operation => return output,
        () = tokio::time::sleep(SLOW_REQUEST) => {
            log!("request {method} still running after {}", elapsed(SLOW_REQUEST));
        }
    }
    let output = operation.await;
    log!(
        "request {method} finished after {}",
        elapsed(started.elapsed())
    );
    output
}

/// A duration in its two largest units, such as `2h 3m` or `45s`.
fn elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match (seconds / 3600, seconds / 60 % 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, minutes, _) => format!("{hours}h {minutes}m"),
    }
}

#[cfg(test)]
mod tests;
