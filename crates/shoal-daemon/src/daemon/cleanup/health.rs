//! How a background loop of passes, such as automatic cleanup's or idle
//! sleep's, is doing: whether its passes fail, in which workspaces, or have
//! stalled, and telling the user once when they start failing.
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;
use std::{collections::HashMap, sync::PoisonError, time::Duration};
use tokio::time::Instant;

use crate::{
    daemon::{
        log,
        manager::Manager,
        notifications::{About, AlertKind},
    },
    time::UnixTime,
};
use shoal_daemon_base::failures::SweepFailures;

/// A pass normally finishes within seconds; none for this long means the
/// loop is stuck or has stopped.
const STALLED_AFTER: Duration = Duration::from_secs(10 * 60);

/// The outcome of recent passes: Shoal's status reports a failing or stalled
/// loop, and workspace status the workspace's own failures.
pub struct Health {
    /// The loop's name, as messages start with it.
    name: &'static str,
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

    fn describe(&self, name: &str) -> String {
        format!(
            "{name} failed {} consecutive passes since {}: {}",
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
    workspace: Option<(SpaceId, SpaceName)>,
    error: String,
}

/// [`Health`] as the loop records it and status reads it; each operation
/// holds the lock only while it runs.
#[derive(Default)]
pub(crate) struct CleanupHealth(std::sync::Mutex<Health>);

impl CleanupHealth {
    /// The health of the loop `name`.
    pub(crate) fn of(name: &'static str) -> Self {
        Self(std::sync::Mutex::new(Health::named(name)))
    }

    fn name(&self) -> &'static str {
        self.lock().name
    }

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

/// The loop whose health [`Health::default`] is.
const AUTOMATIC_CLEANUP: &str = "Automatic cleanup";

impl Default for Health {
    fn default() -> Self {
        Self::named(AUTOMATIC_CLEANUP)
    }
}

impl Health {
    fn named(name: &'static str) -> Self {
        Self {
            name,
            started: Moment::now(),
            finished: None,
            failure: None,
            workspaces: HashMap::new(),
        }
    }

    /// A short description of a loop that fails other than in a workspace,
    /// or has stalled.
    fn problem(&self, now: Instant) -> Option<String> {
        if let Some(failure) = &self.failure {
            Some(failure.describe(self.name))
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
        (self.workspaces.get(id)).map(|failure| failure.describe(self.name))
    }

    fn stalled(&self, now: Instant) -> bool {
        now.duration_since(self.finished.unwrap_or(self.started).at) >= STALLED_AFTER
    }

    fn stalled_message(&self) -> String {
        match self.finished {
            Some(finished) => format!(
                "{} has not finished a pass since {}",
                self.name,
                finished.unix.local_seconds()
            ),
            None => format!(
                "{} has not finished a pass since the daemon started at {}",
                self.name,
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
    /// Why automatic cleanup or idle sleep is not working other than in a
    /// workspace, if one is not.
    pub fn cleanup_problem(&self) -> Option<String> {
        let now = Instant::now();
        (self.cleanup_health.problem(now)).or_else(|| self.idle_sleep_health.problem(now))
    }

    /// Why automatic cleanup or idle sleep fails in the workspace `id`, if
    /// one does.
    pub fn workspace_cleanup_problem(&self, id: &SpaceId) -> Option<String> {
        (self.cleanup_health.workspace_problem(id))
            .or_else(|| self.idle_sleep_health.workspace_problem(id))
    }

    /// Record a finished cleanup sweep with `result`, as [`Self::record_pass`]
    /// does.
    pub(super) async fn record_sweep(&self, result: Result<(), SweepFailures>) {
        self.record_pass(&self.cleanup_health, result).await;
    }

    /// Record a finished idle sleep pass with `result`, as
    /// [`Self::record_pass`] does.
    pub(crate) async fn record_idle_sleep(&self, result: Result<(), SweepFailures>) {
        self.record_pass(&self.idle_sleep_health, result).await;
    }

    /// Record a finished pass of the loop `health` follows, notifying the
    /// user when its passes start failing, about the workspace each failure
    /// concerns. A notification that cannot be recorded, as when the disk is
    /// full, is retried on the next pass.
    async fn record_pass(&self, health: &CleanupHealth, result: Result<(), SweepFailures>) {
        for unreported in health.record(Moment::now(), &result) {
            let about = match &unreported.workspace {
                Some((id, name)) => About::space(name, id),
                None => About::default(),
            };
            let name = health.name().to_lowercase();
            let message = format!("{name} failed: {}", unreported.error);
            let kind = AlertKind::CleanupFailed;
            match self
                .coordination
                .record_notification(self, about, kind, message)
                .await
            {
                Ok(_) => {
                    let id = unreported.workspace.as_ref().map(|(id, _)| id.as_str());
                    health.failure_notified(id);
                }
                Err(error) => log!("notification not recorded: {error:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests;
