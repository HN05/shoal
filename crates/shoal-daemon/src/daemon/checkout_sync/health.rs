//! What a repository's syncs found: when its remotes were last fetched, when
//! its default branch last followed them, and the failure since then.
use crate::client::repository_ids::RepositoryId;
use std::time::Duration;

use anyhow::Error;
use tokio::time::Instant;

use crate::{model::CheckoutSync, time::UnixTime};

/// A sync failing this long needs the user. Measured on the monotonic clock,
/// which excludes sleep, so a Mac that wakes before its network does is not
/// reported for the hours it slept.
const NEEDS_USER_AFTER: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
pub(super) struct Health {
    fetched: Option<Instant>,
    synced_at: Option<UnixTime>,
    failure: Option<Failure>,
}

/// Consecutive failed syncs.
struct Failure {
    since: Instant,
    /// `since` on the wall clock, for display.
    since_unix: UnixTime,
    error: String,
    notified: bool,
}

impl Health {
    pub(super) fn fetched(&mut self, at: Instant) {
        self.fetched = Some(at);
    }

    pub(super) fn fetched_since(&self, since: Instant) -> bool {
        self.fetched.is_some_and(|at| at >= since)
    }

    /// Record a finished sync and its error, if it failed.
    pub(super) fn record(&mut self, error: Option<&Error>, now: Instant, unix: UnixTime) {
        let Some(error) = error else {
            self.synced_at = Some(unix);
            self.failure = None;
            return;
        };
        let failure = self.failure.get_or_insert(Failure {
            since: now,
            since_unix: unix,
            error: String::new(),
            notified: false,
        });
        failure.error = format!("{error:#}");
    }

    /// What the finished syncs found; none before the first one finishes.
    pub(super) fn view(&self, repository_id: &RepositoryId) -> Option<CheckoutSync> {
        (self.synced_at.is_some() || self.failure.is_some()).then(|| CheckoutSync {
            repository_id: repository_id.to_owned(),
            synced_at: self.synced_at,
            failing_since: self.failure.as_ref().map(|failure| failure.since_unix),
            error: self.failure.as_ref().map(|failure| failure.error.clone()),
        })
    }

    pub(super) fn needs_user(&self, now: Instant) -> bool {
        self.failure
            .as_ref()
            .is_some_and(|failure| now.duration_since(failure.since) >= NEEDS_USER_AFTER)
    }

    /// The failure to tell the user about, once it needs them; marks it told.
    pub(super) fn notify(&mut self, now: Instant) -> Option<String> {
        if !self.needs_user(now) {
            return None;
        }
        let failure = self.failure.as_mut()?;
        (!failure.notified).then(|| {
            failure.notified = true;
            failure.error.clone()
        })
    }
}

#[cfg(test)]
mod tests;
