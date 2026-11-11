//! Accepting client connections, the capacity they hold and the descriptors
//! they use.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, bail};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};

use super::log;
use crate::protocol::Method;
use running::Running;

mod running;

/// Most request/response operations running at once. A request past it is
/// refused at once with the requests that hold the capacity, so a few that
/// hang cannot keep the daemon from accepting the rest. Daemon administration
/// needs no slot, so status and shutdown answer even at the limit.
const MAX_REQUESTS: usize = 128;
/// Descriptors kept for the daemon's own files and the commands it runs.
const RESERVED_DESCRIPTORS: usize = 512;
/// Long-lived connections allowed even when the descriptor limit is low.
const MIN_LONG_LIVED: usize = 64;

/// Separate limits for requests and for connections that stay open, such as
/// executions and waits, so those cannot take the capacity of the requests
/// that end them. Connections are accepted whatever either holds.
pub(super) struct Capacity {
    requests: Limit,
    running: Arc<Running>,
    long_lived: Limit,
}

/// One kind of capacity and whether it was full when last asked.
struct Limit {
    permits: Arc<Semaphore>,
    limit: usize,
    /// Set from the first refusal until a connection is admitted again, so
    /// retrying clients log the limit once.
    full: AtomicBool,
}

/// Capacity a connection holds until it closes.
pub(super) struct Slot {
    _permit: Option<OwnedSemaphorePermit>,
    _running: Option<running::Entry>,
}

impl Capacity {
    /// Long-lived connections get what the descriptor limit leaves after
    /// requests and the daemon's own files.
    pub(super) fn for_descriptors(limit: u64) -> Self {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let long_lived = limit
            .saturating_sub(MAX_REQUESTS + RESERVED_DESCRIPTORS)
            .clamp(MIN_LONG_LIVED, Semaphore::MAX_PERMITS);
        Self::new(MAX_REQUESTS, long_lived)
    }

    pub(super) fn new(requests: usize, long_lived: usize) -> Self {
        Self {
            requests: Limit::new(requests),
            running: Arc::default(),
            long_lived: Limit::new(long_lived),
        }
    }

    /// The slot a connection keeps once its request is known: long-lived
    /// capacity for one that stays open, otherwise a request slot.
    pub(super) fn admit(&self, method: &Method) -> Result<Slot> {
        if method.holds_connection() {
            return self.long_lived();
        }
        if method.controls_daemon() {
            return Ok(Slot {
                _permit: None,
                _running: None,
            });
        }
        self.request(method.name())
    }

    /// Long-lived capacity, for a connection that stays open.
    pub(super) fn long_lived(&self) -> Result<Slot> {
        let Some(permit) = self.long_lived.try_acquire() else {
            self.long_lived
                .log_full("refusing long-lived connections", "");
            bail!(
                "the daemon already serves its limit of {} long-lived connections",
                self.long_lived.limit
            );
        };
        Ok(Slot {
            _permit: Some(permit),
            _running: None,
        })
    }

    /// A request slot for the request `name`, while it runs.
    pub(super) fn request(&self, name: String) -> Result<Slot> {
        let Some(permit) = self.requests.try_acquire() else {
            let held = self.running.summary();
            self.requests
                .log_full("refusing requests", &format!("; running: {held}"));
            bail!(
                "the daemon is busy: all {} request slots are held by {held}; retry once they finish, or run `shoal daemon restart` if they hang",
                self.requests.limit
            );
        };
        Ok(Slot {
            _permit: Some(permit),
            _running: Some(self.running.start(name)),
        })
    }
}

impl Limit {
    fn new(limit: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit)),
            limit,
            full: AtomicBool::new(false),
        }
    }

    fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        let permit = self.permits.clone().try_acquire_owned().ok()?;
        self.full.store(false, Ordering::Relaxed);
        Some(permit)
    }

    /// Log the first refusal since the last admission.
    fn log_full(&self, refusing: &str, details: &str) {
        if !self.full.swap(true, Ordering::Relaxed) {
            log!("{refusing} at the limit of {}{details}", self.limit);
        }
    }
}

/// Open files the daemon asks for: room for thousands of connections, within
/// the per-process limit macOS accepts (`OPEN_MAX`).
const DESCRIPTOR_TARGET: u64 = 10_240;

/// Raise the soft open-file limit toward the hard limit, since launchd starts
/// services with 256, and return the limit in effect.
pub(super) fn raise_descriptor_limit() -> u64 {
    let Some(limit) = descriptor_limit() else {
        return DESCRIPTOR_TARGET;
    };
    let target = limit.rlim_max.min(DESCRIPTOR_TARGET);
    if limit.rlim_cur >= target {
        return limit.rlim_cur;
    }
    let raised = libc::rlimit {
        rlim_cur: target,
        rlim_max: limit.rlim_max,
    };
    // SAFETY: `raised` is a valid rlimit for the duration of the call.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
        return target;
    }
    let error = io::Error::last_os_error();
    log!(
        "raise open file limit from {} to {target}: {error}",
        limit.rlim_cur
    );
    limit.rlim_cur
}

fn descriptor_limit() -> Option<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid, writable rlimit for the duration of the call.
    (unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0).then_some(limit)
}

/// Wait before accepting again after a failure that clears on its own.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// The next client connection. Running out of descriptors or a client that
/// hung up before acceptance passes, so those failures wait and retry instead
/// of stopping the daemon.
pub(super) async fn accept(listener: &UnixListener) -> io::Result<UnixStream> {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if transient(&error) => {
                log!("accept connection: {error}; retrying");
                tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted | io::ErrorKind::Interrupted
    ) || matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    )
}

#[cfg(test)]
mod tests;
