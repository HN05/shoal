//! What a machine's reports carry of its capacity and power. The machine
//! measures them beside its reports, so a report never waits for a
//! measurement.
use std::{convert::Infallible, time::Duration};

use anyhow::Result;

use shoal_protocol::hub::sleep::Power;
use shoal_schedule::placement::snapshot::Capacity;
use tokio::sync::watch;

use super::summaries::Origins;
use crate::{Hub, battery, daemon::Daemon, log};

/// How often a machine measures its capacity.
const MEASURE_EVERY: Duration = Duration::from_secs(15);

/// How far the memory new work may use moves before a report goes.
const MEMORY_STEP: u64 = 1 << 30;

/// What a machine last measured of itself: its capacity, none before it
/// measured it, and how it is powered, none without a battery.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Measured {
    pub capacity: Option<Capacity>,
    pub power: Option<Power>,
}

impl Hub {
    /// Measure this machine's capacity and power into `measured` every
    /// [`MEASURE_EVERY`], and at once when [told it changed](Self::capacity_changed).
    /// Receivers hear of a measurement when the capacity moved enough to place
    /// by, when the power changed, and the first time. A failed measurement of
    /// the capacity keeps the one before.
    pub(super) async fn keep_measuring(
        &self,
        daemon: &dyn Daemon,
        measured: watch::Sender<Measured>,
    ) -> Infallible {
        let mut origins = Origins::default();
        loop {
            let capacity = match self.measure_capacity(daemon, &mut origins).await {
                Ok(capacity) => Some(capacity),
                Err(error) => {
                    log!("hub: could not measure this machine's queue for placement: {error:#}");
                    None
                }
            };
            let power = off_runtime(battery::power).await;
            measured.send_if_modified(|current| {
                let moved = capacity.as_ref().is_some_and(|capacity| {
                    (current.capacity.as_ref()).is_none_or(|current| moved(current, capacity))
                });
                let changed = moved || current.power != power;
                current.capacity = capacity.or(current.capacity.take());
                current.power = power;
                changed
            });
            tokio::select! {
                () = tokio::time::sleep(MEASURE_EVERY) => {}
                () = self.remeasure.notified() => {}
            }
        }
    }

    /// Measure this machine's capacity again at once, since its admission
    /// changed, as once its user paused or unpaused it.
    pub fn capacity_changed(&self) {
        self.remeasure.notify_one();
    }

    /// This machine's capacity now: its queue's room as `daemon` reads it,
    /// with the origins of its repositories.
    async fn measure_capacity(
        &self,
        daemon: &dyn Daemon,
        origins: &mut Origins,
    ) -> Result<Capacity> {
        let room = daemon.queue_room().await?;
        let mut repositories = Vec::new();
        for repository in daemon.repositories().await? {
            repositories.extend(origins.of(&repository).await.origin);
        }
        repositories.sort();
        repositories.dedup();
        Ok(Capacity {
            repositories,
            ..room
        })
    }
}

/// `read`, run off the runtime, none when it could not run.
async fn off_runtime<T: Send + 'static>(read: fn() -> Option<T>) -> Option<T> {
    tokio::task::spawn_blocking(read).await.ok().flatten()
}

/// Whether `after` differs from `before` enough to place by: in anything
/// but the memory new work may use, or in that by [`MEMORY_STEP`].
fn moved(before: &Capacity, after: &Capacity) -> bool {
    let memory = |capacity: &Capacity| capacity.memory_free.map(|memory| memory / MEMORY_STEP);
    let without_memory = |capacity: &Capacity| Capacity {
        memory_free: None,
        ..capacity.clone()
    };
    memory(before) != memory(after) || without_memory(before) != without_memory(after)
}
