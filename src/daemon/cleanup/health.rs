//! How automatic cleanup is doing: whether its passes fail, in which
//! workspaces, or have stalled, and telling the user once when they start
//! failing.
use crate::client::space_ids::SpaceId;
use std::{collections::HashMap, sync::PoisonError, time::Duration};
use tokio::time::Instant;

use super::failures::SweepFailures;
use crate::{
    daemon::{
        log,
        notifications::{About, NotificationKind},
        workspace::Manager,
    },
    time::UnixTime,
};

/// A pass normally finishes within seconds; none for this long means the
/// loop is stuck or has stopped.
const STALLED_AFTER: Duration = Duration::from_secs(10 * 60);

/// The outcome of recent passes: daemon status reports a failing or stalled
/// loop, and workspace status the workspace's own failures.
pub struct Health {
    started: Moment,
    finished: Option<Moment>,
    /// Consecutive passes with failures that concern no workspace.
    failure: Option<Failure>,
    /// Each workspace's consecutive passes with failures, by its ID.
    workspaces: HashMap<SpaceId, Failure>,
}

/// A point in time measured both ways: the monotonic clock excludes sleep, so
/// a Mac waking up does not look like a stalled loop, and the wall-clock time
/// is what messages show.
#[derive(Clone, Copy)]
struct Moment {
    at: Instant,
    unix: UnixTime,
}

impl Moment {
    fn now() -> Self {
        Self {
            at: Instant::now(),
            unix: UnixTime::now(),
        }
    }
}

/// Consecutive failed passes.
struct Failure {
    since: UnixTime,
    passes: u64,
    error: String,
    notified: bool,
}

impl Failure {
    /// The failure after another pass that failed with `error` at `now`.
    fn continued(previous: Option<Self>, now: Moment, error: String) -> Self {
        let mut failure = previous.unwrap_or(Failure {
            since: now.unix,
            passes: 0,
            error: String::new(),
            notified: false,
        });
        failure.passes += 1;
        failure.error = error;
        failure
    }

    fn describe(&self) -> String {
        format!(
            "Automatic cleanup failed {} consecutive passes since {}: {}",
            self.passes,
            self.since.local_seconds(),
            self.error
        )
    }
}

/// A failure the user has not been told about yet.
#[derive(Debug, PartialEq, Eq)]
struct Unreported {
    /// The ID and name of the workspace it concerns, if any.
    workspace: Option<(SpaceId, String)>,
    error: String,
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

    fn workspace_problem(&self, id: &SpaceId) -> Option<String> {
        self.lock().workspace_problem(id)
    }

    fn record(&self, now: Moment, result: &Result<(), SweepFailures>) -> Vec<Unreported> {
        self.lock().record(now, result)
    }

    /// The user was told about the current failure of the workspace `id`,
    /// or of none.
    fn failure_notified(&self, id: Option<&str>) {
        let mut health = self.lock();
        let failure = match id {
            Some(id) => health.workspaces.get_mut(id),
            None => health.failure.as_mut(),
        };
        if let Some(failure) = failure {
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
            workspaces: HashMap::new(),
        }
    }
}

impl Health {
    /// A short description of a loop that fails other than in a workspace,
    /// or has stalled.
    fn problem(&self, now: Instant) -> Option<String> {
        if let Some(failure) = &self.failure {
            Some(failure.describe())
        } else if self.stalled(now) {
            Some(format!(
                "{}; the daemon log has details",
                self.stalled_message()
            ))
        } else {
            None
        }
    }

    /// A short description of the failures in the workspace `id`.
    fn workspace_problem(&self, id: &SpaceId) -> Option<String> {
        self.workspaces.get(id).map(Failure::describe)
    }

    fn stalled(&self, now: Instant) -> bool {
        now.duration_since(self.finished.unwrap_or(self.started).at) >= STALLED_AFTER
    }

    fn stalled_message(&self) -> String {
        match self.finished {
            Some(finished) => format!(
                "Automatic cleanup has not finished a pass since {}",
                finished.unix.local_seconds()
            ),
            None => format!(
                "Automatic cleanup has not finished a pass since the daemon started at {}",
                self.started.unix.local_seconds()
            ),
        }
    }

    /// Record a finished pass; returns the failures to tell the user about
    /// that have not been reported yet.
    fn record(&mut self, now: Moment, result: &Result<(), SweepFailures>) -> Vec<Unreported> {
        self.finished = Some(now);
        let none = SweepFailures::default();
        let failures = result.as_ref().err().unwrap_or(&none);
        let mut unreported = Vec::new();
        self.failure = (failures.general()).map(|error| {
            let failure = Failure::continued(self.failure.take(), now, error);
            if !failure.notified {
                unreported.push(Unreported {
                    workspace: None,
                    error: failure.error.clone(),
                });
            }
            failure
        });
        let mut previous = std::mem::take(&mut self.workspaces);
        for (id, name, error) in failures.spaces() {
            let failure = Failure::continued(previous.remove(id), now, error);
            if !failure.notified {
                unreported.push(Unreported {
                    workspace: Some((id.to_owned(), name.to_owned())),
                    error: failure.error.clone(),
                });
            }
            self.workspaces.insert(id.to_owned(), failure);
        }
        unreported
    }
}

impl Manager {
    /// Why automatic cleanup is not working other than in a workspace, if it
    /// is not.
    pub fn cleanup_problem(&self) -> Option<String> {
        self.cleanup_health.problem(Instant::now())
    }

    /// Why automatic cleanup fails in the workspace `id`, if it does.
    pub fn workspace_cleanup_problem(&self, id: &SpaceId) -> Option<String> {
        self.cleanup_health.workspace_problem(id)
    }

    /// Notify the user when passes start failing, about the workspace each
    /// failure concerns. A notification that cannot be recorded, as when the
    /// disk is full, is retried on the next pass.
    pub(super) async fn record_sweep(&self, result: Result<(), SweepFailures>) {
        for unreported in self.cleanup_health.record(Moment::now(), &result) {
            let about = match &unreported.workspace {
                Some((id, name)) => About::space(name, id),
                None => About::default(),
            };
            let message = format!("automatic cleanup failed: {}", unreported.error);
            match self
                .record_notification(about, NotificationKind::CleanupFailed, message)
                .await
            {
                Ok(_) => {
                    let id = unreported.workspace.as_ref().map(|(id, _)| id.as_str());
                    self.cleanup_health.failure_notified(id);
                }
                Err(error) => log!("notification not recorded: {error:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests;
