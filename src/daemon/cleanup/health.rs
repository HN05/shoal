//! How automatic cleanup is doing: whether its passes fail or have stalled,
//! and telling the user once when they start failing.
use anyhow::Result;
use std::{sync::PoisonError, time::Duration};
use tokio::time::Instant;

use crate::{
    daemon::{log, notifications::NotificationKind, workspace::Manager},
    time::{local_seconds, unix_seconds},
};

/// A pass normally finishes within seconds; none for this long means the
/// loop is stuck or has stopped.
const STALLED_AFTER: Duration = Duration::from_secs(10 * 60);

/// The outcome of recent passes, which workspace status reports.
pub struct Health {
    started: Moment,
    finished: Option<Moment>,
    failure: Option<Failure>,
}

/// A point in time measured both ways: the monotonic clock excludes sleep, so
/// a Mac waking up does not look like a stalled loop, and the Unix seconds
/// are what messages show.
#[derive(Clone, Copy)]
struct Moment {
    at: Instant,
    unix: u64,
}

impl Moment {
    fn now() -> Self {
        Self {
            at: Instant::now(),
            unix: unix_seconds(),
        }
    }
}

/// Consecutive failed passes.
struct Failure {
    since: u64,
    passes: u64,
    error: String,
    notified: bool,
}

/// [`Health`] as the sweep records it and status reads it; each operation
/// holds the lock only while it runs.
#[derive(Default)]
pub(crate) struct CleanupHealth(std::sync::Mutex<Health>);

impl CleanupHealth {
    fn lock(&self) -> std::sync::MutexGuard<'_, Health> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn problem(&self, now: Instant) -> Option<String> {
        self.lock().problem(now)
    }

    fn record(&self, now: Moment, result: &Result<()>) -> Option<String> {
        self.lock().record(now, result)
    }

    /// The user was told about the current failure.
    fn failure_notified(&self) {
        if let Some(failure) = &mut self.lock().failure {
            failure.notified = true;
        }
    }
}

impl Default for Health {
    fn default() -> Self {
        Self {
            started: Moment::now(),
            finished: None,
            failure: None,
        }
    }
}

impl Health {
    /// A short description of a failing or stalled loop for workspace status.
    fn problem(&self, now: Instant) -> Option<String> {
        if let Some(failure) = &self.failure {
            Some(format!(
                "Automatic cleanup failed {} consecutive passes since {}: {}",
                failure.passes,
                local_seconds(failure.since as i64),
                failure.error
            ))
        } else if self.stalled(now) {
            Some(format!(
                "{}; the daemon log has details",
                self.stalled_message()
            ))
        } else {
            None
        }
    }

    fn stalled(&self, now: Instant) -> bool {
        now.duration_since(self.finished.unwrap_or(self.started).at) >= STALLED_AFTER
    }

    fn stalled_message(&self) -> String {
        match self.finished {
            Some(finished) => format!(
                "Automatic cleanup has not finished a pass since {}",
                local_seconds(finished.unix as i64)
            ),
            None => format!(
                "Automatic cleanup has not finished a pass since the daemon started at {}",
                local_seconds(self.started.unix as i64)
            ),
        }
    }

    /// Record a finished pass; returns the error to tell the user about when
    /// a failure has not been reported yet.
    fn record(&mut self, now: Moment, result: &Result<()>) -> Option<String> {
        self.finished = Some(now);
        let Err(error) = result else {
            self.failure = None;
            return None;
        };
        let failure = self.failure.get_or_insert(Failure {
            since: now.unix,
            passes: 0,
            error: String::new(),
            notified: false,
        });
        failure.passes += 1;
        failure.error = format!("{error:#}");
        (!failure.notified).then(|| failure.error.clone())
    }
}

impl Manager {
    /// Why automatic cleanup is not working, if it is not.
    pub fn cleanup_problem(&self) -> Option<String> {
        self.cleanup_health.problem(Instant::now())
    }

    /// Notify the user when passes start failing. A notification that cannot
    /// be recorded, as when the disk is full, is retried on the next pass.
    pub(super) async fn record_sweep(&self, result: Result<()>) {
        let Some(error) = self.cleanup_health.record(Moment::now(), &result) else {
            return;
        };
        let message = format!("automatic cleanup failed: {error}");
        match self
            .record_notification(Default::default(), NotificationKind::CleanupFailed, message)
            .await
        {
            Ok(_) => self.cleanup_health.failure_notified(),
            Err(error) => log!("notification not recorded: {error:#}"),
        }
    }
}

#[cfg(test)]
mod tests;
