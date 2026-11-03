//! The time runs of messages settle by: the wall clock and Tokio's timer when
//! the daemon runs. A test can stop it and move it by hand, so no test depends
//! on when the next second begins.
use std::time::Duration;

use anyhow::Result;

#[derive(Default)]
pub(crate) struct SettleClock {
    #[cfg(test)]
    stopped: std::sync::Mutex<Option<Stopped>>,
    #[cfg(test)]
    sleeping: tokio::sync::Notify,
}

impl SettleClock {
    /// The current Unix second.
    pub(super) fn unix_seconds(&self) -> Result<i64> {
        #[cfg(test)]
        if let Some(stopped) = &*self.stopped.lock().unwrap() {
            return Ok(stopped.now);
        }
        Ok(i64::try_from(crate::time::unix_seconds())?)
    }

    /// Return once `duration`, in whole seconds, has passed.
    pub(super) async fn sleep(&self, duration: Duration) {
        #[cfg(test)]
        if let Some(woken) = self.stopped_sleep(duration) {
            let _ = woken.await;
            return;
        }
        tokio::time::sleep(duration).await;
    }

    /// [`Self::sleep`] for `hold`, or never return without one.
    pub(crate) async fn wait_out(&self, hold: Option<Duration>) {
        match hold {
            Some(hold) => self.sleep(hold).await,
            None => std::future::pending().await,
        }
    }
}

#[cfg(test)]
struct Stopped {
    now: i64,
    sleepers: Vec<Sleeper>,
}

#[cfg(test)]
struct Sleeper {
    end: i64,
    wake: tokio::sync::oneshot::Sender<()>,
}

#[cfg(test)]
impl SettleClock {
    /// Stop the clock at the current second; only [`Self::advance`] moves it
    /// after.
    pub(crate) fn stop(&self) {
        let now = self.unix_seconds().unwrap();
        *self.stopped.lock().unwrap() = Some(Stopped {
            now,
            sleepers: Vec::new(),
        });
    }

    /// Wait until something sleeps on the stopped clock, then move it to the
    /// end of the earliest sleep, waking every sleep that ends by then.
    pub(crate) async fn advance(&self) {
        loop {
            let sleeping = self.sleeping.notified();
            {
                let mut stopped = self.stopped.lock().unwrap();
                let stopped = stopped.as_mut().expect("the clock is stopped");
                stopped.sleepers.retain(|sleeper| !sleeper.wake.is_closed());
                if let Some(end) = stopped.sleepers.iter().map(|sleeper| sleeper.end).min() {
                    stopped.now = stopped.now.max(end);
                    let now = stopped.now;
                    let (done, waiting) = std::mem::take(&mut stopped.sleepers)
                        .into_iter()
                        .partition::<Vec<_>, _>(|sleeper| sleeper.end <= now);
                    stopped.sleepers = waiting;
                    for sleeper in done {
                        let _ = sleeper.wake.send(());
                    }
                    return;
                }
            }
            sleeping.await;
        }
    }

    /// A sleep on the stopped clock, or `None` while it runs.
    fn stopped_sleep(&self, duration: Duration) -> Option<tokio::sync::oneshot::Receiver<()>> {
        let (wake, woken) = tokio::sync::oneshot::channel();
        {
            let mut stopped = self.stopped.lock().unwrap();
            let stopped = stopped.as_mut()?;
            let end = stopped.now + i64::try_from(duration.as_secs()).unwrap();
            stopped.sleepers.push(Sleeper { end, wake });
        }
        self.sleeping.notify_waiters();
        Some(woken)
    }
}
