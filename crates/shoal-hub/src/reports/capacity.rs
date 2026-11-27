//! What a machine's reports carry of its capacity, runs and power. The
//! machine measures them beside its reports, so a report never waits for a
//! measurement.
use std::{collections::BTreeMap, convert::Infallible, time::Duration};

use anyhow::Result;

use shoal_daemon_base::run_usage::{MedianDuration, median_durations};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{hub::sleep::Power, model::Repository};
use shoal_schedule::placement::{snapshot::Capacity, speeds::Medians};
use tokio::sync::watch;

use super::summaries::Origins;
use crate::{Hub, battery, daemon::Daemon, log, time::UnixTime};

/// How often a machine measures its capacity.
const MEASURE_EVERY: Duration = Duration::from_secs(15);

/// How far the memory new work may use moves before a report goes.
const MEMORY_STEP: u64 = 1 << 30;

/// How far back the runs a machine's medians read ended.
const RUNS_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

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
    /// with its repositories and the agents it can launch.
    async fn measure_capacity(
        &self,
        daemon: &dyn Daemon,
        origins: &mut Origins,
    ) -> Result<Capacity> {
        let room = daemon.queue_room().await?;
        let launchable = self.launchable(daemon, origins).await?;
        Ok(Capacity {
            repositories: launchable.repositories,
            agents: launchable.agents,
            ..room
        })
    }

    /// The medians of this machine's runs over the last [`RUNS_WINDOW`], by
    /// the origins of their repositories.
    async fn measure_medians(&self, daemon: &dyn Daemon, origins: &mut Origins) -> Result<Medians> {
        let origins = by_origin(&daemon.repositories().await?, origins).await;
        let since = UnixTime::now() - RUNS_WINDOW;
        let medians = (self.store)
            .read(move |db| median_durations(db, since))
            .await?;
        Ok(by_repository(medians, &origins))
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
/// but the memory new work may use, or in that by [`MEMORY_STEP`].
fn moved(before: &Capacity, after: &Capacity) -> bool {
    let memory = |capacity: &Capacity| capacity.memory_free.map(|memory| memory / MEMORY_STEP);
    let without_memory = |capacity: &Capacity| Capacity {
        memory_free: None,
        ..capacity.clone()
    };
    memory(before) != memory(after) || without_memory(before) != without_memory(after)
}
