//! The time the proxy waits and budgets by: tokio's when it runs, one a
//! test moves by hand in its tests, so no test depends on how fast it runs.
use std::{future::Future, pin::Pin, time::Duration};

pub(crate) type Sleep = Pin<Box<dyn Future<Output = ()> + Send>>;

pub(crate) trait Clock: Send + Sync {
    /// The time since the clock started.
    fn now(&self) -> Duration;

    /// The wall-clock time in Unix seconds, which an upstream's reset times
    /// are given in.
    fn unix_seconds(&self) -> u64;

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

    fn unix_seconds(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// A clock that moves only when a test advances it, to the end of the
/// earliest wait, and only once something waits on it, so it never moves
/// while the proxy waits on a socket; a deadline passes only when the test
/// expires it.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Manual {
    state: std::sync::Mutex<ManualState>,
    sleeping: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Default)]
struct ManualState {
    now: Duration,
    sleepers: Vec<Sleeper>,
}

#[cfg(test)]
struct Sleeper {
    end: Duration,
    deadline: bool,
    wake: tokio::sync::oneshot::Sender<()>,
}

/// The Unix second a [`Manual`] clock starts at.
#[cfg(test)]
pub(crate) const MANUAL_UNIX_START: u64 = 1_700_000_000;

#[cfg(test)]
impl Manual {
    /// Wait until something waits, then move to the end of the earliest
    /// wait, waking everything that ends by then.
    pub(crate) async fn advance(&self) {
        self.move_to(|sleeper| !sleeper.deadline).await;
    }

    /// Wait until something waits, without moving.
    pub(crate) async fn waiting(&self) {
        loop {
            let sleeping = self.sleeping.notified();
            {
                let state = self.state.lock().unwrap();
                if state
                    .sleepers
                    .iter()
                    .any(|s| !s.deadline && !s.wake.is_closed())
                {
                    return;
                }
            }
            sleeping.await;
        }
    }

    /// [`Manual::advance`] to the earliest wait or deadline.
    pub(crate) async fn expire(&self) {
        self.move_to(|_| true).await;
    }

    async fn move_to(&self, target: impl Fn(&Sleeper) -> bool) {
        loop {
            let sleeping = self.sleeping.notified();
            {
                let mut state = self.state.lock().unwrap();
                state.sleepers.retain(|sleeper| !sleeper.wake.is_closed());
                let earliest = state
                    .sleepers
                    .iter()
                    .filter(|s| target(s))
                    .map(|s| s.end)
                    .min();
                if let Some(end) = earliest {
                    state.now = state.now.max(end);
                    let now = state.now;
                    let (done, waiting) = std::mem::take(&mut state.sleepers)
                        .into_iter()
                        .partition::<Vec<_>, _>(|sleeper| sleeper.end <= now);
                    state.sleepers = waiting;
                    for sleeper in done {
                        let _ = sleeper.wake.send(());
                    }
                    return;
                }
            }
            sleeping.await;
        }
    }

    fn sleep_until(&self, duration: Duration, deadline: bool) -> Sleep {
        let (wake, woken) = tokio::sync::oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            let end = state.now + duration;
            state.sleepers.push(Sleeper {
                end,
                deadline,
                wake,
            });
        }
        self.sleeping.notify_waiters();
        Box::pin(async {
            let _ = woken.await;
        })
    }
}

#[cfg(test)]
impl Clock for Manual {
    fn now(&self) -> Duration {
        self.state.lock().unwrap().now
    }

    fn unix_seconds(&self) -> u64 {
        MANUAL_UNIX_START + self.now().as_secs()
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        self.sleep_until(duration, false)
    }

    fn deadline(&self, duration: Duration) -> Sleep {
        self.sleep_until(duration, true)
    }
}
