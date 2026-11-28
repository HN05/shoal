//! What this machine's queue has room for now, which it reports to the hub for
//! placement.
use std::{collections::HashSet, time::Duration};

use crate::state::QueueKind;
use anyhow::Result;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_schedule::placement::snapshot::Capacity;

use super::{
    limits::{self, Limits, Waiting, Wake, Work},
    store::{QueuedRow, queued_rows},
    weight::Light,
};
use crate::Agents;
use crate::capacity::HostSample;
use crate::capacity::MemoryState;
use crate::capacity::Room;
use crate::daemon::Daemon;

/// The most heavy starts the report counts, however high a fixed limit is.
const COUNTED: u32 = 64;

impl Agents {
    /// What this machine's queue has room for now, without its
    /// repositories, which the hub's reports read on their own.
    pub async fn queue_room(&self, daemon: &dyn Daemon) -> Result<Capacity> {
        let config = self.config.current();
        let light = self.light(daemon).await?;
        let slots = self.occupied_slots(daemon, &HashSet::new(), &light).await?;
        let jobs = daemon.job_activities().await?;
        let mut limits = self.agent_limits(daemon, Vec::new(), &slots, &jobs).await?;
        let (activities, _) = self.activities(&slots, &jobs);
        let overload = &config.machine.overload;
        let (sample, room) = self.measure(daemon, overload, &activities).await?;
        // The heavy slot and the memory free go by one reading.
        if limits.room.is_some() {
            limits.room = Some(room.clone());
        }
        let occupied: Vec<_> = slots.into_iter().map(|slot| slot.repository).collect();
        Ok(Capacity {
            repositories: Vec::new(),
            agents: Vec::new(),
            max_heavy_agents: config.machine.max_heavy_agents.fixed(),
            heavy_free: heavy_free(&occupied, &limits),
            light_free: light_free(&occupied, &limits),
            memory_free: memory_free(sample, room),
            heavy_waiting: self.heavy_waiting(daemon, &light).await?,
            away: self.agent_queue.held_away(),
            paused: self.agent_queue.paused_since(),
        })
    }

    /// The heavy agents waiting in the queue for room, which `light` tells
    /// from the light ones.
    async fn heavy_waiting(&self, daemon: &dyn Daemon, light: &Light) -> Result<u32> {
        let queued = self.store.run(|db| queued_rows(db)).await?;
        let heavy = self.waiting_heavy(daemon, &queued, light).await?.len();
        Ok(heavy.try_into().unwrap_or(u32::MAX))
    }

    /// The entries among `queued` that wait for room to start a heavy
    /// agent, which `light` tells from the light ones.
    pub(super) async fn waiting_heavy<'a>(
        &self,
        daemon: &dyn Daemon,
        queued: &'a [QueuedRow],
        light: &'a Light,
    ) -> Result<Vec<&'a QueuedRow>> {
        let mut waiting = Vec::new();
        for row in heavy_entries(queued, light) {
            if self.entry_waits_for_room(daemon, row).await? {
                waiting.push(row);
            }
        }
        Ok(waiting)
    }

    /// Whether the entry `row` waits for room, which a restore does unless
    /// it waits for an update that has not come.
    async fn entry_waits_for_room(&self, daemon: &dyn Daemon, row: &QueuedRow) -> Result<bool> {
        let (QueueKind::Restore, Some(execution), Some(workspace)) =
            (row.kind, &row.execution, &row.workspace)
        else {
            return Ok(true);
        };
        let wake = (self.restore_wake(daemon, workspace, execution, row.automatic)).await?;
        Ok(waits_for_room(wake))
    }
}

/// The entries among `queued` that start a heavy agent, which `light`
/// tells from the light ones.
fn heavy_entries<'a>(
    queued: &'a [QueuedRow],
    light: &'a Light,
) -> impl Iterator<Item = &'a QueuedRow> {
    (queued.iter())
        .filter(|row| row.kind != QueueKind::Job)
        .filter(|row| {
            !light.entry(
                row.execution.as_ref(),
                row.repository.as_ref(),
                row.mode.as_ref(),
            )
        })
}

/// Whether a restore that `wake` admits waits for room: one that waits for
/// an update that has not come holds back nothing until then.
fn waits_for_room(wake: Wake) -> bool {
    wake != Wake::Update { due: false }
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
        place: None,
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

#[cfg(test)]
mod tests;
