//! Dynamic agent capacity from the daemon's host measurements.
//!
//! Agents start one at a time while measured load leaves room for another.
//! An agent first reads files and only later edits and compiles, so a recent
//! start counts at the load a settled agent shows until it settles itself.
//! Fixed limits apply the same free disk space rule.
use std::{path::Path, time::Duration};

use crate::{config::overload::Overload, fsutil, process::load};

/// How long a new agent counts at a settled agent's load instead of its own.
pub(crate) const WARM_UP: Duration = Duration::from_secs(3 * 60);
/// The least time between dynamic starts, so their first reads do not collide.
pub(crate) const STAGGER: Duration = Duration::from_secs(10);
/// The window current load is averaged over.
pub(crate) const RECENT: Duration = Duration::from_secs(60);
/// The window a settled agent's load is learned over.
pub(crate) const SETTLED: Duration = Duration::from_secs(5 * 60);
/// The load assumed for an agent before any has settled, in cores.
const ASSUMED_AGENT_CORES: f64 = 2.0;
/// The least load an agent is assumed to add, in cores.
const MIN_AGENT_CORES: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HostSample {
    pub parallelism: u32,
    pub cpu: Option<CpuSample>,
    pub memory: MemoryState,
    pub disk_free: Option<u64>,
}

/// Busy percentage across all cores over [`RECENT`] and [`SETTLED`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CpuSample {
    pub recent: f64,
    pub settled: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryState {
    Healthy,
    Warning,
    Critical,
    Unavailable,
}

/// The agents holding slots, by how far their load shows in measurements.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Agents {
    /// Every slot, including stopped agents awaiting resume.
    pub total: u32,
    /// Starting entries and agents running for less than [`WARM_UP`].
    pub warming: u32,
    /// Agents running for at least [`WARM_UP`].
    pub settled: u32,
    /// Time since the newest agent or start began.
    pub newest: Option<Duration>,
}

/// What one used slot does to measured load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Activity {
    /// An admitted start whose agent has not registered, since this long.
    Starting(Duration),
    /// A connected agent, running this long.
    Running(Duration),
    /// An agent awaiting reattachment or resume, which runs no load here.
    Stopped,
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
                Activity::Running(age) if age < WARM_UP => {
                    agents.warming += 1;
                    age
                }
                Activity::Running(age) => {
                    agents.settled += 1;
                    age
                }
                Activity::Stopped => continue,
            };
            agents.newest = Some(agents.newest.map_or(age, |newest| newest.min(age)));
        }
        agents
    }
}

/// Probe the same memory and disk sources used by overload protection. The
/// first path is daemon state; later paths are workspaces awaiting cleanup.
/// A failed probe never authorizes more work; deleted workspaces are skipped.
pub(crate) fn sample(paths: &[&Path], settings: &Overload, cpu: Option<CpuSample>) -> HostSample {
    HostSample {
        parallelism: std::thread::available_parallelism()
            .map(|value| value.get().min(u32::MAX as usize) as u32)
            .unwrap_or(1)
            .max(1),
        cpu,
        memory: sample_memory(settings),
        disk_free: disk_free(paths, settings),
    }
}

/// The least free space across `paths`, as [`sample`] probes it.
pub(crate) fn disk_free(paths: &[&Path], settings: &Overload) -> Option<u64> {
    sample_disk(paths, settings.disk.enabled, fsutil::available_bytes)
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

/// The machine-wide limit: the slots in use, plus one when another agent may
/// start now. Critical or unavailable memory and disk readings start none;
/// otherwise an idle machine, where no agent runs or starts, always starts one.
pub(crate) fn limit(sample: HostSample, agents: Agents, settings: &Overload) -> u32 {
    if blocked(sample, settings) {
        return 0;
    }
    let idle = agents.warming + agents.settled == 0;
    let start =
        idle || (agents.newest.is_none_or(|age| age >= STAGGER) && room(sample, agents, settings));
    agents.total + u32::from(start)
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

/// Whether one more agent fits below the warning thresholds once every
/// warming agent shows a settled agent's load.
fn room(sample: HostSample, agents: Agents, settings: &Overload) -> bool {
    let Some(cpu) = sample.cpu else {
        return false;
    };
    let memory = sample.memory == MemoryState::Healthy;
    let disk = disk(sample.disk_free, settings) == Disk::Healthy;
    let per_agent = agent_load(cpu, agents.settled, sample.parallelism);
    let projected = cpu.recent + per_agent * f64::from(agents.warming + 1);
    memory && disk && projected < f64::from(settings.warning.cpu_used_percent)
}

/// One agent's share of settled load as a busy percentage, assuming
/// [`ASSUMED_AGENT_CORES`] until an agent settles. The user's own work counts
/// toward it, which only makes admission more careful.
fn agent_load(cpu: CpuSample, settled: u32, parallelism: u32) -> f64 {
    let cores = |cores: f64| cores * 100.0 / f64::from(parallelism);
    if settled == 0 {
        cores(ASSUMED_AGENT_CORES)
    } else {
        (cpu.settled / f64::from(settled)).max(cores(MIN_AGENT_CORES))
    }
}

#[cfg(test)]
mod tests;
