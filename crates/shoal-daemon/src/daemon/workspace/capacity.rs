//! Dynamic admission from the daemon's memory and disk readings.
//!
//! Agents and CI jobs start one at a time while memory and disk leave room
//! for another. CPU does not limit them: builds and tests use every core that
//! is free, and more work on a busy CPU only runs slower, while memory and
//! disk do not stretch. An agent first reads files and only later edits and
//! compiles, so a recent start counts at the memory a settled agent's
//! workspace uses until it settles itself; a recent job counts at the memory
//! its earlier runs reached. Fixed limits apply the same free disk space rule.
use std::{path::Path, time::Duration};

pub(crate) use crate::client::executions::STAGGER;
use crate::{config::overload::Overload, fsutil, model::HeldBy, process::load};

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

/// What a CI job is expected to use: the resident memory its earlier runs
/// reached, `None` before it has run, when it counts as an agent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct JobCost {
    pub memory: Option<u64>,
}

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
    /// The memory the jobs running for less than [`WARM_UP`] reached in
    /// earlier runs, and how many of them have not run before.
    pub warming_job_memory: u64,
    pub warming_new_jobs: u32,
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
    /// A CI job admitted this long ago, expected to use `cost`.
    Job { age: Duration, cost: JobCost },
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
    /// warming agent and job uses what it is expected to, and what an
    /// agent's start is expected to use.
    Free { memory: u64, agent: u64 },
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
        let mut agents = Self::default();
        for activity in activities {
            agents.total += 1;
            let age = match activity {
                Activity::Starting(age) => {
                    agents.warming += 1;
                    age
                }
                Activity::Running { age, .. } if age < WARM_UP => {
                    agents.warming += 1;
                    age
                }
                Activity::Running { age, memory } => {
                    agents.settled += 1;
                    if let Some(memory) = memory {
                        agents.settled_memory = agents.settled_memory.saturating_add(memory);
                        agents.measured += 1;
                    }
                    age
                }
                Activity::Job { age, cost } => {
                    agents.jobs += 1;
                    if age < WARM_UP {
                        match cost.memory {
                            Some(memory) => {
                                agents.warming_job_memory =
                                    agents.warming_job_memory.saturating_add(memory)
                            }
                            None => agents.warming_new_jobs += 1,
                        }
                    }
                    age
                }
                Activity::Stopped => continue,
            };
            agents.newest = Some(agents.newest.map_or(age, |newest| newest.min(age)));
        }
        agents
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

    /// The memory the warming agents and jobs are expected to use, each
    /// agent and new job at `agent`.
    fn warming_memory(&self, agent: u64) -> u64 {
        agent
            .saturating_mul(u64::from(self.warming + self.warming_new_jobs))
            .saturating_add(self.warming_job_memory)
    }
}

/// Probe the same memory and disk sources used by overload protection. The
/// first path is daemon state; later paths are workspaces awaiting cleanup.
/// A failed probe never authorizes more work; deleted workspaces are skipped.
pub(crate) fn sample(paths: &[&Path], settings: &Overload) -> HostSample {
    HostSample {
        memory: sample_memory(settings),
        memory_room: memory_room(settings),
        disk_free: disk_free(paths, settings),
    }
}

/// The bytes new work may use before memory reaches the warning threshold;
/// unlimited with memory protection off.
fn memory_room(settings: &Overload) -> Option<u64> {
    if !settings.memory.enabled {
        return Some(u64::MAX);
    }
    let memory = load::memory().ok()?;
    let warning = u128::from(memory.total) * u128::from(settings.warning.memory_used_percent) / 100;
    let used = u128::from(memory.total - memory.available);
    Some(warning.saturating_sub(used).try_into().unwrap_or(u64::MAX))
}

/// The least free space across `paths`, as [`sample`] probes it.
pub(crate) fn disk_free(paths: &[&Path], settings: &Overload) -> Option<u64> {
    sample_disk(
        paths,
        settings.disk.enabled,
        fsutil::disk_space::available_bytes,
    )
}

fn sample_memory(settings: &Overload) -> MemoryState {
    if !settings.memory.enabled {
        return MemoryState::Healthy;
    }
    match load::critical_memory(settings.memory.used_percent) {
        Ok(true) => MemoryState::Critical,
        Ok(false) => match load::memory_warning(settings.warning.memory_used_percent) {
            Ok(true) => MemoryState::Warning,
            Ok(false) => MemoryState::Healthy,
            Err(_) => MemoryState::Unavailable,
        },
        Err(_) => MemoryState::Unavailable,
    }
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
/// warning threshold once every warming agent and job uses what it is
/// expected to. Critical or unavailable memory and disk readings leave none;
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
        memory: free.saturating_sub(agents.warming_memory(agent)),
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
