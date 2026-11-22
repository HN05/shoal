//! What a machine's reports carry of its capacity, which the hub places
//! new spaces by: the origins of its repositories and its queue's room,
//! measured again at most every [`MEASURE_EVERY`]. A report carries the
//! latest whenever it goes, and a change enough to place by sends one.
use std::time::Duration;

use shoal_schedule::placement::snapshot::Capacity;
use tokio::time::Instant;

use super::summaries::Origins;
use crate::{Hub, daemon::Daemon, log};

/// How often a machine measures its capacity again.
pub(super) const MEASURE_EVERY: Duration = Duration::from_secs(15);

/// How far the memory new work may use moves before a report goes.
const MEMORY_STEP: u64 = 1 << 30;

/// A machine's capacity as it last measured it, and when.
pub(super) struct Measured {
    pub capacity: Capacity,
    at: Instant,
}

impl Measured {
    /// When it is measured again.
    pub fn due(&self) -> Instant {
        self.at + MEASURE_EVERY
    }
}

impl Hub {
    /// This machine's capacity now, its queue's room as `daemon` reads it,
    /// or as `before` had it when that fails, with the origins of its
    /// repositories.
    pub(super) async fn measure_capacity(
        &self,
        daemon: &dyn Daemon,
        origins: &mut Origins,
        before: Option<&Measured>,
    ) -> Measured {
        let room = match daemon.queue_room().await {
            Ok(room) => room,
            Err(error) => {
                log!("hub: could not measure this machine's queue for placement: {error:#}");
                (before.map(|measured| measured.capacity.clone())).unwrap_or_default()
            }
        };
        let mut repositories = Vec::new();
        for repository in daemon.repositories().await.unwrap_or_default() {
            repositories.extend(origins.of(&repository).await.origin);
        }
        repositories.sort();
        repositories.dedup();
        Measured {
            capacity: Capacity {
                repositories,
                ..room
            },
            at: Instant::now(),
        }
    }
}

/// Whether `after` differs from `before` enough to place by: in anything
/// but the memory new work may use, or in that by [`MEMORY_STEP`].
pub(super) fn moved(before: &Capacity, after: &Capacity) -> bool {
    let memory = |capacity: &Capacity| capacity.memory_free.map(|memory| memory / MEMORY_STEP);
    let without_memory = |capacity: &Capacity| Capacity {
        memory_free: None,
        ..capacity.clone()
    };
    memory(before) != memory(after) || without_memory(before) != without_memory(after)
}
