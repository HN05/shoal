//! The clock the proxy's tests move by hand.
use std::time::Duration;

use shoal_time::UnixTime;

use super::{Clock, Sleep};

/// A clock that moves only when a test advances it, to the end of the
/// earliest wait, and only once something waits on it, so it never moves
/// while the proxy waits on a socket; a deadline passes only when the test
/// expires it.
#[derive(Default)]
pub(crate) struct Manual {
    state: std::sync::Mutex<ManualState>,
    sleeping: tokio::sync::Notify,
}

#[derive(Default)]
struct ManualState {
    now: Duration,
    sleepers: Vec<Sleeper>,
}

struct Sleeper {
    end: Duration,
    deadline: bool,
    wake: tokio::sync::oneshot::Sender<()>,
}

/// The wall-clock time a [`Manual`] clock starts at.
pub(crate) const MANUAL_UNIX_START: UnixTime = UnixTime::from_seconds(1_700_000_000);

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

impl Clock for Manual {
    fn now(&self) -> Duration {
        self.state.lock().unwrap().now
    }

    fn unix_time(&self) -> UnixTime {
        MANUAL_UNIX_START + self.now()
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        self.sleep_until(duration, false)
    }

    fn deadline(&self, duration: Duration) -> Sleep {
        self.sleep_until(duration, true)
    }
}
