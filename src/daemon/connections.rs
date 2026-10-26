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

use anyhow::{Result, anyhow};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};

use super::log;
use crate::protocol::Method;

/// Most connections reading their request or running a request/response
/// operation at once.
const MAX_REQUESTS: usize = 128;
/// Descriptors kept for the daemon's own files and the commands it runs.
const RESERVED_DESCRIPTORS: usize = 512;
/// Long-lived connections allowed even when the descriptor limit is low.
const MIN_LONG_LIVED: usize = 64;

/// Separate limits for requests and for connections that stay open, such as
/// executions and waits, so those cannot take the capacity of the requests
/// that end them.
pub(super) struct Capacity {
    requests: Arc<Semaphore>,
    long_lived: Arc<Semaphore>,
    long_lived_limit: usize,
    /// Set from the first refusal until a long-lived connection is admitted
    /// again, so retrying clients log the limit once.
    long_lived_full: AtomicBool,
}

/// Capacity a connection holds until it closes.
pub(super) struct Slot {
    _permit: OwnedSemaphorePermit,
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
            requests: Arc::new(Semaphore::new(requests)),
            long_lived: Arc::new(Semaphore::new(long_lived)),
            long_lived_limit: long_lived,
            long_lived_full: AtomicBool::new(false),
        }
    }

    /// The next client connection, once a request slot is free.
    pub(super) async fn accept(&self, listener: &UnixListener) -> io::Result<(UnixStream, Slot)> {
        let permit = self
            .requests
            .clone()
            .acquire_owned()
            .await
            .expect("request capacity is never closed");
        Ok((accept(listener).await?, Slot { _permit: permit }))
    }

    /// The slot a connection keeps once its request is known. One that stays
    /// open moves into long-lived capacity, freeing its request slot.
    pub(super) fn admit(&self, request: Slot, method: &Method) -> Result<Slot> {
        if !method.holds_connection() {
            return Ok(request);
        }
        let Ok(permit) = self.long_lived.clone().try_acquire_owned() else {
            if !self.long_lived_full.swap(true, Ordering::Relaxed) {
                log!(
                    "refusing long-lived connections at the limit of {}",
                    self.long_lived_limit
                );
            }
            return Err(anyhow!(
                "the daemon already serves its limit of {} long-lived connections",
                self.long_lived_limit
            ));
        };
        self.long_lived_full.store(false, Ordering::Relaxed);
        drop(request);
        Ok(Slot { _permit: permit })
    }

    /// A request slot for a connection served without a listener.
    #[cfg(test)]
    pub(super) fn request_slot(&self) -> Slot {
        let permit = self.requests.clone().try_acquire_owned();
        Slot {
            _permit: permit.expect("a free request slot"),
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
async fn accept(listener: &UnixListener) -> io::Result<UnixStream> {
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
