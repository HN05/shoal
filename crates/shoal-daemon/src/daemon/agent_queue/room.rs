//! What this machine's queue has room for now, which it reports to the hub
//! so the hub can place new spaces: the heavy and light agents admission
//! would start, the memory new work may use and the heavy agents waiting,
//! as one admission pass would find them.
use std::{collections::HashSet, time::Duration};

use crate::state::QueueKind;
use anyhow::Result;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_schedule::placement::snapshot::Capacity;

use super::{
    limits::{self, Limits, Waiting, Work},
    store::queued_rows,
    weight::Light,
};
use crate::daemon::{
    capacity::{HostSample, MemoryState, Room},
    manager::Manager,
};

/// The most heavy starts the report counts, however high a fixed limit is.
const COUNTED: u32 = 64;

impl Manager {
    /// What this machine's queue has room for now, without its
    /// repositories, which the hub's reports read on their own.
    pub(crate) async fn queue_room(&self) -> Result<Capacity> {
        let config = self.config();
        let light = self.light().await?;
        let slots = self.occupied_slots(&HashSet::new(), &light).await?;
        let jobs = self.job_activities().await?;
        let limits = self.agent_limits(Vec::new(), &slots, &jobs).await?;
        let (activities, _) = self.activities(&slots, &jobs);
        let overload = &config.machine.overload;
        let (sample, room) = self.measure(overload, &activities).await?;
        let occupied: Vec<_> = slots.into_iter().map(|slot| slot.repository).collect();
        Ok(Capacity {
            repositories: Vec::new(),
            max_heavy_agents: config.machine.max_heavy_agents.fixed(),
            heavy_free: heavy_free(&occupied, &limits),
            light_free: light_free(&occupied, &limits),
            memory_free: memory_free(sample, room),
            heavy_waiting: self.heavy_waiting(&light).await?,
        })
    }

    /// The heavy agents waiting in the queue, which `light` tells from the
    /// light ones.
    async fn heavy_waiting(&self, light: &Light) -> Result<u32> {
        let queued = self.store.run(|db| queued_rows(db)).await?;
        let heavy = (queued.iter())
            .filter(|row| row.kind != QueueKind::Job)
            .filter(|row| {
                !light.entry(
                    row.execution.as_ref(),
                    row.repository.as_ref(),
                    row.mode.as_ref(),
                )
            })
            .count();
        Ok(heavy.try_into().unwrap_or(u32::MAX))
    }
}

/// The heavy launches `limits` would admit beside the `occupied` slots, up
/// to [`COUNTED`]: under dynamic admission at most one, as one pass starts.
fn heavy_free(
    occupied: &[Option<shoal_ids::repository_ids::RepositoryId>],
    limits: &Limits,
) -> u32 {
    let probes = limits.global.unwrap_or(1).min(COUNTED);
    let probes: Vec<Waiting> = (0..probes).map(|index| probe(index, false)).collect();
    let admitted = limits::admit(occupied, &probes, limits, false)
        .admitted
        .len();
    admitted.try_into().unwrap_or(u32::MAX)
}

/// Whether `limits` would admit a light launch beside the `occupied`
/// slots.
fn light_free(
    occupied: &[Option<shoal_ids::repository_ids::RepositoryId>],
    limits: &Limits,
) -> bool {
    !(limits::admit(occupied, &[probe(0, true)], limits, false).admitted).is_empty()
}

/// A launch admission weighs, `light` or heavy, in no repository.
fn probe(index: u32, light: bool) -> Waiting {
    Waiting {
        id: QueueEntryId::from(format!("probe-{index}")),
        repository: None,
        work: Work::Launch,
        light,
        waited_for_memory: Duration::ZERO,
    }
}

/// The bytes of memory new work may use under `room`, read from `sample`:
/// none where a reading is critical or missing or disk holds new work back,
/// all the reading leaves while nothing runs, and unknown while a start is
/// too recent to count.
fn memory_free(sample: HostSample, room: Room) -> Option<u64> {
    match room {
        Room::Free { memory, .. } => Some(memory),
        Room::Held(_) => Some(0),
        Room::Staggered => None,
        Room::Idle => match sample.memory {
            MemoryState::Healthy => sample.memory_room,
            MemoryState::Warning | MemoryState::Critical => Some(0),
            MemoryState::Unavailable => None,
        },
    }
}
