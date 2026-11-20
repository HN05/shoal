//! Dynamic admission from the daemon's memory, disk and CPU readings.
//!
//! Agents and CI jobs start one at a time while memory and disk leave room
//! for another. CPU holds back only a job beside running ones: builds and
//! tests use every core that is free, so no start reserves CPU, but jobs
//! piled on a saturated CPU only make each other slower, while memory and
//! disk do not stretch. An agent first reads files and only later edits and
//! compiles, so a recent start counts at the memory a settled agent's
//! workspace uses until it settles itself. A job reaches its peak late, while
//! it builds and runs tests, so until it ends it reserves what its earlier
//! runs reached beyond what it uses now. Fixed limits apply the same free disk
//! space rule.
use std::{path::Path, time::Duration};

use crate::{config::overload::Overload, fsutil, model::HeldBy, process::load};
pub(crate) use shoal_protocol::executions::STAGGER;

/// How long a new agent counts at a settled agent's memory instead of its own.
pub(crate) const WARM_UP: Duration = Duration::from_secs(3 * 60);
/// The memory a starting agent counts at before any agent has settled.
const ASSUMED_AGENT_MEMORY: u64 = 2 << 30;
/// The least memory a starting agent counts at.
const MIN_AGENT_MEMORY: u64 = 1 << 30;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HostSample {
    pub memory: MemoryState,
    /// The bytes new work may use before memory reaches its warning
    /// threshold; `None` without a reading.
    pub memory_room: Option<u64>,
    pub disk_free: Option<u64>,
}

pub(crate) use shoal_runner::jobs::JobCost;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryState {
    Healthy,
    Warning,
    Critical,
    Unavailable,
}

/// The agents and CI jobs holding slots, by how far their memory shows in
/// measurements.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Agents {
    /// Every slot, including stopped agents awaiting resume.
    pub total: u32,
    /// Starting entries and agents running for less than [`WARM_UP`].
    pub warming: u32,
    /// Agents running for at least [`WARM_UP`].
    pub settled: u32,
    /// Jobs starting or running.
    pub jobs: u32,
    /// The memory the settled agents' workspaces used at the latest sample,
    /// and how many of them it counts.
    pub settled_memory: u64,
    pub measured: u32,
    /// The memory the jobs starting or running have yet to use: what each
    /// is expected to use beyond what its workspace used at the latest
    /// sample.
    pub job_reservation: u64,
    /// Time since the newest agent, job or start began.
    pub newest: Option<Duration>,
}

/// What one used slot does to measured memory.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Activity {
    /// An admitted start whose agent has not registered, since this long.
    Starting(Duration),
    /// A connected agent, running for `age`, whose workspace used `memory`
    /// at the latest usage sample, `None` without one.
    Running { age: Duration, memory: Option<u64> },
    /// An agent awaiting reattachment or resume, which runs nothing here.
    Stopped,
    /// A CI job admitted this long ago, expected to use `cost`, whose
    /// workspace used `memory` at the latest usage sample, `None` without
    /// one.
    Job {
        age: Duration,
        cost: JobCost,
        memory: Option<u64>,
    },
}

/// What measured memory and disk leave for one more start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Room {
    /// Nothing starts until the last start is [`STAGGER`] old.
    Staggered,
    /// Nothing starts: a reading is critical or missing, or disk is past its
    /// warning.
    Held(HeldBy),
    /// Nothing runs or starts, so one starts whatever memory is in use.
    Idle,
    /// The bytes of memory left below the warning threshold once each
    /// warming agent uses what it is expected to and each job what it has
    /// yet to use, and what an agent's start is expected to use.
    Free { memory: u64, agent: u64 },
}

/// The machine's busy CPU, niced time included, averaged over the CPU
/// warning's window, `None` until a reading spans it, and that warning's
/// level, at which a job beside running ones waits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Cpu {
    pub busy: Option<f64>,
    pub level: u8,
}

impl Cpu {
    /// Whether a job fits beside `running` jobs: the first whatever the CPU
    /// reads, so agents' load never starves jobs, and a further one only
    /// below the level.
    pub(crate) fn fits_job(self, running: u32) -> Result<(), HeldBy> {
        if running == 0 {
            return Ok(());
        }
        match self.busy {
            None => Err(HeldBy::Reading),
            Some(busy) if busy >= f64::from(self.level) => Err(HeldBy::Cpu {
                busy: busy as u8,
                level: self.level,
            }),
            Some(_) => Ok(()),
        }
    }
}

/// Why a start does not fit: what holds it, `None` for the stagger, which is
/// too brief to report.
pub(crate) type Unfit = Option<HeldBy>;

impl Room {
    /// Whether an agent fits.
    pub(crate) fn fits_agent(self) -> Result<(), Unfit> {
        self.fits(None)
    }

    /// Whether a job expected to use `cost` fits.
    pub(crate) fn fits_job(self, cost: JobCost) -> Result<(), Unfit> {
        self.fits(cost.memory)
    }

    /// Whether a start using `memory`, or an agent's without it, fits.
    fn fits(self, memory: Option<u64>) -> Result<(), Unfit> {
        match self {
            Self::Staggered => Err(None),
            Self::Held(held) => Err(Some(held)),
            Self::Idle => Ok(()),
            Self::Free {
                memory: left,
                agent,
            } => {
                let needed = memory.unwrap_or(agent);
                match needed < left {
                    true => Ok(()),
                    false => Err(Some(HeldBy::Memory { needed, left })),
                }
            }
        }
    }
}

impl Agents {
    pub(crate) fn count(activities: impl IntoIterator<Item = Activity>) -> Self {
        let activities: Vec<Activity> = activities.into_iter().collect();
        let mut agents = Self::default();
        for activity in &activities {
            agents.add(*activity);
        }
        // A new job counts at an agent's start, which the agents set.
        agents.job_reservation = agents.job_reservation(&activities);
        agents
    }

    /// Count one used slot.
    fn add(&mut self, activity: Activity) {
        self.total += 1;
        let age = match activity {
            Activity::Starting(age) => {
                self.warming += 1;
                age
            }
            Activity::Running { age, .. } if age < WARM_UP => {
                self.warming += 1;
                age
            }
            Activity::Running { age, memory } => {
                self.settled += 1;
                if let Some(memory) = memory {
                    self.settled_memory = self.settled_memory.saturating_add(memory);
                    self.measured += 1;
                }
                age
            }
            Activity::Job { age, .. } => {
                self.jobs += 1;
                age
            }
            Activity::Stopped => return,
        };
        self.newest = Some(self.newest.map_or(age, |newest| newest.min(age)));
    }

    /// The memory the jobs among `activities` have yet to use: each one's
    /// expected memory, or an agent's start before it has run, less what its
    /// workspace used at the latest sample, the whole of it without one. The
    /// machine's memory reading already holds what a job uses.
    fn job_reservation(&self, activities: &[Activity]) -> u64 {
        let agent = self.agent_memory();
        activities
            .iter()
            .filter_map(|activity| match *activity {
                Activity::Job { cost, memory, .. } => Some(
                    cost.memory
                        .unwrap_or(agent)
                        .saturating_sub(memory.unwrap_or(0)),
                ),
                _ => None,
            })
            .fold(0, u64::saturating_add)
    }

    /// The memory an agent's start counts at: what the settled agents'
    /// workspaces use on average, at least [`MIN_AGENT_MEMORY`], or
    /// [`ASSUMED_AGENT_MEMORY`] before any has settled.
    pub(crate) fn agent_memory(&self) -> u64 {
        match self.measured {
            0 => ASSUMED_AGENT_MEMORY,
            measured => (self.settled_memory / u64::from(measured)).max(MIN_AGENT_MEMORY),
        }
    }

    /// The memory the warming agents, each at `agent`, and the jobs are
    /// expected to use beyond what the machine's reading holds.
    fn reserved_memory(&self, agent: u64) -> u64 {
        agent
            .saturating_mul(u64::from(self.warming))
            .saturating_add(self.job_reservation)
    }
}

/// Probe the same memory and disk sources used by overload protection. The
/// first path is daemon state; later paths are workspaces awaiting cleanup.
/// A failed probe never authorizes more work; deleted workspaces are skipped.
pub(crate) fn sample(paths: &[&Path], settings: &Overload) -> HostSample {
    let (memory, memory_room) = sample_memory(settings);
    HostSample {
        memory,
        memory_room,
        disk_free: disk_free(paths, settings),
    }
}

/// The memory state and room of one reading: healthy and unlimited with
/// memory protection off, unavailable without room after a failed reading.
fn sample_memory(settings: &Overload) -> (MemoryState, Option<u64>) {
    if !settings.memory.enabled {
        return (MemoryState::Healthy, Some(u64::MAX));
    }
    match load::memory(&settings.memory_levels()) {
        Ok(reading) => (
            memory_state(reading.pressure),
            Some(memory_room(reading.memory, settings)),
        ),
        Err(_) => (MemoryState::Unavailable, None),
    }
}

/// Stall that has yet to hold counts as critical here: it starts nothing,
/// though it stops nothing yet.
fn memory_state(pressure: load::MemoryPressure) -> MemoryState {
    if pressure.critical || pressure.stalled {
        MemoryState::Critical
    } else if pressure.warning {
        MemoryState::Warning
    } else {
        MemoryState::Healthy
    }
}

/// The bytes new work may use before `memory` in use reaches the warning
/// threshold.
fn memory_room(memory: load::Memory, settings: &Overload) -> u64 {
    let warning = u128::from(memory.total) * u128::from(settings.warning.memory_used_percent) / 100;
    let used = u128::from(memory.total - memory.available);
    warning.saturating_sub(used).try_into().unwrap_or(u64::MAX)
}

/// The free space admission reads in place of the host's, once a test sets
/// it to keep admission off the free space of the machine running it.
#[cfg(any(test, feature = "test-support"))]
static FAKE_DISK_FREE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Read `free` bytes as every filesystem's free space for admission from
/// now on, in this process.
#[cfg(any(test, feature = "test-support"))]
pub fn fake_disk_free(free: u64) {
    FAKE_DISK_FREE.get_or_init(|| free);
}

/// The least free space across `paths`, as [`sample`] probes it.
pub(crate) fn disk_free(paths: &[&Path], settings: &Overload) -> Option<u64> {
    sample_disk(paths, settings.disk.enabled, available)
}

/// The free space of `path`'s filesystem, or what a test set in its place.
fn available(path: &Path) -> std::io::Result<u64> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(free) = FAKE_DISK_FREE.get() {
        return Ok(*free);
    }
    fsutil::disk_space::available_bytes(path)
}

fn sample_disk(
    paths: &[&Path],
    enabled: bool,
    available: impl Fn(&Path) -> std::io::Result<u64>,
) -> Option<u64> {
    if !enabled {
        return Some(u64::MAX);
    }
    paths
        .iter()
        .enumerate()
        .try_fold(None::<u64>, |minimum, (index, path)| {
            match available(path) {
                Ok(free) => Some(Some(minimum.map_or(free, |current| current.min(free)))),
                Err(error) if index > 0 && error.kind() == std::io::ErrorKind::NotFound => {
                    Some(minimum)
                }
                Err(_) => None,
            }
        })
        .flatten()
}

/// What measured memory and disk leave for one more start beside `agents`,
/// which a start fits when its memory stays below what is left under the
/// warning threshold once every warming agent uses what it is expected to
/// and every job what it has yet to use. Critical or unavailable memory and disk readings leave none;
/// an idle machine, where nothing runs or starts, always starts one.
pub(crate) fn room(sample: HostSample, agents: Agents, settings: &Overload) -> Room {
    if blocked(sample, settings) {
        return Room::Held(HeldBy::Reading);
    }
    if agents.warming + agents.settled + agents.jobs == 0 {
        return Room::Idle;
    }
    if agents.newest.is_some_and(|age| age < STAGGER) {
        return Room::Staggered;
    }
    if disk(sample.disk_free, settings) != Disk::Healthy {
        return Room::Held(HeldBy::Disk);
    }
    let Some(free) = sample.memory_room else {
        return Room::Held(HeldBy::Reading);
    };
    // Memory past its warning leaves nothing, whatever the percentage says.
    let free = match sample.memory {
        MemoryState::Healthy => free,
        _ => 0,
    };
    let agent = agents.agent_memory();
    Room::Free {
        memory: free.saturating_sub(agents.reserved_memory(agent)),
        agent,
    }
}

/// The machine-wide limit free disk space sets beside a fixed limit, for
/// `total` slots in use: none below the stop threshold or without a reading,
/// the slots in use below the warning threshold but one when none is used,
/// and no limit otherwise.
pub(crate) fn disk_limit(free: Option<u64>, total: u32, settings: &Overload) -> Option<u32> {
    match disk(free, settings) {
        Disk::Healthy => None,
        Disk::Low => Some(total.max(1)),
        Disk::Critical => Some(0),
    }
}

/// Whether `free` disk space holds back new starts: below the warning
/// threshold, or unread.
pub(crate) fn disk_low(free: Option<u64>, settings: &Overload) -> bool {
    disk(free, settings) != Disk::Healthy
}

/// Free disk space against the overload thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disk {
    Healthy,
    Low,
    Critical,
}

fn disk(free: Option<u64>, settings: &Overload) -> Disk {
    match free {
        Some(free) if free >= settings.disk.stop_free_bytes() => {
            if free >= settings.warning.disk_free_bytes() {
                Disk::Healthy
            } else {
                Disk::Low
            }
        }
        _ => Disk::Critical,
    }
}

fn blocked(sample: HostSample, settings: &Overload) -> bool {
    matches!(
        sample.memory,
        MemoryState::Critical | MemoryState::Unavailable
    ) || disk(sample.disk_free, settings) == Disk::Critical
}

#[cfg(test)]
mod tests;
