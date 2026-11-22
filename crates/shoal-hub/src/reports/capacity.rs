//! What a machine's reports carry of its capacity, which the hub places
//! new spaces by: the origins of its repositories and its queue's room,
//! which the machine measures every [`MEASURE_EVERY`] beside its reports,
//! so a report never waits for a measurement, and a change enough to place
//! by sends one.
use std::{convert::Infallible, time::Duration};

use anyhow::Result;

use shoal_schedule::placement::snapshot::Capacity;
use tokio::sync::watch;

use super::summaries::Origins;
use crate::{Hub, daemon::Daemon, log};

/// How often a machine measures its capacity.
const MEASURE_EVERY: Duration = Duration::from_secs(15);

/// How far the memory new work may use moves before a report goes.
const MEMORY_STEP: u64 = 1 << 30;

impl Hub {
    /// Measure this machine's capacity every [`MEASURE_EVERY`] into
    /// `measured`, whose receivers hear of it once it moved enough to place
    /// by, first once it is measured at all. A failed measurement keeps
    /// the one before.
    pub(super) async fn keep_measuring(
        &self,
        daemon: &dyn Daemon,
        measured: watch::Sender<Option<Capacity>>,
    ) -> Infallible {
        let mut origins = Origins::default();
        loop {
            match self.measure_capacity(daemon, &mut origins).await {
                Ok(capacity) => {
                    measured.send_if_modified(|current| {
                        let moved = current
                            .as_ref()
                            .is_none_or(|current| moved(current, &capacity));
                        *current = Some(capacity);
                        moved
                    });
                }
                Err(error) => {
                    log!("hub: could not measure this machine's queue for placement: {error:#}");
                }
            }
            tokio::time::sleep(MEASURE_EVERY).await;
        }
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
