//! The time the proxy waits and budgets by: tokio's when it runs, one a
//! test moves by hand in its tests, so no test depends on how fast it runs.
use std::{future::Future, pin::Pin, time::Duration};

use shoal_time::UnixTime;

#[cfg(test)]
mod manual;
#[cfg(test)]
pub(crate) use manual::{MANUAL_UNIX_START, Manual};

pub(crate) type Sleep = Pin<Box<dyn Future<Output = ()> + Send>>;

pub(crate) trait Clock: Send + Sync {
    /// The time since the clock started.
    fn now(&self) -> Duration;

    /// The wall-clock time, which an upstream's reset times are given in.
    fn unix_time(&self) -> UnixTime;

    /// A future that completes once `duration` has passed: a wait between
    /// attempts.
    fn sleep(&self, duration: Duration) -> Sleep;

    /// [`Clock::sleep`] as the bound of work in progress, which a test
    /// moves past only on purpose.
    fn deadline(&self, duration: Duration) -> Sleep {
        self.sleep(duration)
    }
}

pub(crate) struct Tokio(tokio::time::Instant);

impl Tokio {
    pub(crate) fn new() -> Self {
        Self(tokio::time::Instant::now())
    }
}

impl Clock for Tokio {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }

    fn unix_time(&self) -> UnixTime {
        UnixTime::now()
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        Box::pin(tokio::time::sleep(duration))
    }
}
