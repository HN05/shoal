//! What a machine's reports carry of its capacity, runs and power. The
//! machine measures them beside its reports, so a report never waits for a
//! measurement.
use std::{collections::BTreeMap, convert::Infallible, time::Duration};

use anyhow::Result;

use shoal_daemon_base::run_usage::{MedianDuration, median_durations};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{hub::sleep::Power, model::Repository};
use shoal_schedule::placement::{
    snapshot::{Capacity, Demand},
    speeds::Medians,
};
use tokio::sync::watch;

use super::summaries::Origins;
use crate::{Hub, battery, daemon::Daemon, log, time::UnixTime};

/// How often a machine measures its capacity.
const MEASURE_EVERY: Duration = Duration::from_secs(15);

/// How far the memory new work may use moves before a report goes.
const MEMORY_STEP: u64 = 1 << 30;

/// How far the time a repository kept the machine busy moves before a
/// report goes.
const BUSY_STEP: Duration = Duration::from_secs(60 * 60);

/// What a machine last measured of itself: its capacity and the medians of
/// its runs, each none before it measured it, and how it is powered, none
/// without a battery.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Measured {
    pub capacity: Option<Capacity>,
    pub medians: Option<Medians>,
    pub power: Option<Power>,
}

/// What a machine measured of itself, each part none where it could not.
struct Measurement {
    capacity: Option<Capacity>,
    medians: Option<Medians>,
    power: Option<Power>,
}

impl Hub {
    /// Measure this machine's capacity, runs and power into `measured` every
    /// [`MEASURE_EVERY`], and at once when [told it changed](Self::capacity_changed).
    /// Receivers hear of a measurement when the capacity moved enough to place
    /// by, when the medians or the power changed, and the first time. A failed
    /// measurement of the capacity or the medians keeps the one before.
    pub(super) async fn keep_measuring(
        &self,
        daemon: &dyn Daemon,
        measured: watch::Sender<Measured>,
    ) -> Infallible {
        let mut origins = Origins::default();
        loop {
            let measurement = self.measure(daemon, &mut origins).await;
            measured.send_if_modified(|current| current.take(measurement));
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

    /// What this machine measures of itself now, logging what it could not.
    async fn measure(&self, daemon: &dyn Daemon, origins: &mut Origins) -> Measurement {
        let capacity = self.measure_capacity(daemon, origins).await;
        let medians = self.measure_medians(daemon, origins).await;
        Measurement {
            capacity: logged(capacity, "queue"),
            medians: logged(medians, "runs"),
            power: off_runtime(battery::power).await,
        }
    }

    /// This machine's capacity now: its queue's room as `daemon` reads it,
    /// with its repositories, the agents it can launch and what each
    /// repository asks of it.
    async fn measure_capacity(
        &self,
        daemon: &dyn Daemon,
        origins: &mut Origins,
    ) -> Result<Capacity> {
        let room = daemon.queue_room().await?;
        let mut launchable = self.launchable(daemon, origins).await?;
        let demand = self.demand_by_origin(daemon, origins).await?;
        for registered in &mut launchable.repositories {
            registered.demand = demand.get(&registered.origin).copied().unwrap_or_default();
        }
        Ok(Capacity {
            repositories: launchable.repositories,
            agents: launchable.agents,
            ..room
        })
    }

    /// The medians of this machine's runs, by the origins of their
    /// repositories.
    async fn measure_medians(&self, daemon: &dyn Daemon, origins: &mut Origins) -> Result<Medians> {
        let origins = by_origin(&daemon.repositories().await?, origins).await;
        let now = UnixTime::now();
        let medians = (self.store)
            .read(move |db| median_durations(db, now))
            .await?;
        Ok(by_repository(medians, &origins))
    }

    /// What the repositories of each origin ask of this machine, summed
    /// over its registrations of one origin.
    async fn demand_by_origin(
        &self,
        daemon: &dyn Daemon,
        origins: &mut Origins,
    ) -> Result<BTreeMap<String, Demand>> {
        let origins = by_origin(&daemon.repositories().await?, origins).await;
        let mut demand = BTreeMap::new();
        for (id, asked) in daemon.repository_demand().await? {
            if let Some(origin) = origins.get(&id) {
                *demand.entry(origin.clone()).or_default() += asked;
            }
        }
        Ok(demand)
    }
}

impl Measured {
    /// Take `measurement` in, keeping what it lacks; whether receivers hear
    /// of it.
    fn take(&mut self, measurement: Measurement) -> bool {
        let Measurement {
            capacity,
            medians,
            power,
        } = measurement;
        let moved = capacity.as_ref().is_some_and(|capacity| {
            (self.capacity.as_ref()).is_none_or(|current| moved(current, capacity))
        });
        let ran = medians.is_some() && medians != self.medians;
        let changed = moved || ran || self.power != power;
        self.capacity = capacity.or(self.capacity.take());
        self.medians = medians.or(self.medians.take());
        self.power = power;
        changed
    }
}

/// The origin of each of `repositories` that has one.
async fn by_origin(
    repositories: &[Repository],
    origins: &mut Origins,
) -> BTreeMap<RepositoryId, String> {
    let mut by_origin = BTreeMap::new();
    for repository in repositories {
        if let Some(origin) = origins.of(repository).await.origin {
            by_origin.insert(repository.id.clone(), origin);
        }
    }
    by_origin
}

/// `medians` by the origins of their repositories, leaving out those of a
/// repository without one, which matches no other machine's.
fn by_repository(
    medians: Vec<MedianDuration>,
    origins: &BTreeMap<RepositoryId, String>,
) -> Medians {
    let mut by_origin = Medians::default();
    for median in medians {
        if let Some(origin) = origins.get(&median.repository) {
            let runs = by_origin.0.entry(origin.clone()).or_default();
            runs.insert(median.run, median.seconds);
        }
    }
    by_origin
}

/// What `measured` holds, none where it failed, which the log records as
/// a failure to measure this machine's `what`.
fn logged<T>(measured: Result<T>, what: &str) -> Option<T> {
    (measured.map_err(|error| {
        log!("hub: could not measure this machine's {what} for placement: {error:#}");
    }))
    .ok()
}

/// `read`, run off the runtime, none when it could not run.
async fn off_runtime<T: Send + 'static>(read: fn() -> Option<T>) -> Option<T> {
    tokio::task::spawn_blocking(read).await.ok().flatten()
}

/// Whether `after` differs from `before` enough to place by: in anything
/// but the memory new work may use and how long each repository kept the
/// machine busy, or in those by [`MEMORY_STEP`] and [`BUSY_STEP`].
fn moved(before: &Capacity, after: &Capacity) -> bool {
    coarse(before) != coarse(after)
}

/// `capacity` with the memory new work may use in [`MEMORY_STEP`]s and
/// each repository's busy time in [`BUSY_STEP`]s.
fn coarse(capacity: &Capacity) -> Capacity {
    let mut coarse = capacity.clone();
    coarse.memory_free = (coarse.memory_free).map(|memory| memory / MEMORY_STEP);
    for registered in &mut coarse.repositories {
        registered.demand.busy_seconds /= BUSY_STEP.as_secs();
    }
    coarse
}

#[cfg(test)]
mod tests;
