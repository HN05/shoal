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
/// otherwise an idle machine always starts one.
pub(crate) fn limit(sample: HostSample, agents: Agents, settings: &Overload) -> u32 {
    if blocked(sample, settings) {
        return 0;
    }
    let start = agents.total == 0
        || (agents.newest.is_none_or(|age| age >= STAGGER) && room(sample, agents, settings));
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
mod tests {
    use super::*;

    fn sample(cpu: f64) -> HostSample {
        HostSample {
            parallelism: 16,
            cpu: Some(CpuSample {
                recent: cpu,
                settled: cpu,
            }),
            memory: MemoryState::Healthy,
            disk_free: Some(u64::MAX),
        }
    }

    fn agents(warming: u32, settled: u32, newest: u64) -> Agents {
        Agents {
            total: warming + settled,
            warming,
            settled,
            newest: Some(Duration::from_secs(newest)),
        }
    }

    #[test]
    fn disk_pressure_includes_workspaces_and_requires_complete_readings() {
        let state = Path::new("/state");
        let workspace = Path::new("/external/workspace");
        let paths = [state, workspace];
        assert_eq!(
            sample_disk(&paths, true, |path| Ok(if path == state { 100 } else { 1 })),
            Some(1),
        );
        assert_eq!(
            sample_disk(&paths, true, |path| {
                if path == state {
                    Ok(100)
                } else {
                    Err(std::io::ErrorKind::PermissionDenied.into())
                }
            }),
            None,
        );
        assert_eq!(
            sample_disk(&paths, false, |_| panic!("disabled disk probe")),
            Some(u64::MAX),
        );
        assert_eq!(
            sample_disk(&paths, true, |path| {
                if path == state {
                    Ok(100)
                } else {
                    Err(std::io::ErrorKind::NotFound.into())
                }
            }),
            Some(100),
        );
        assert_eq!(
            sample_disk(&paths, true, |_| Err(std::io::ErrorKind::NotFound.into())),
            None,
        );
    }

    #[test]
    fn slots_count_by_how_far_their_load_shows() {
        let minute = Duration::from_secs(60);
        assert_eq!(
            Agents::count([
                Activity::Starting(minute * 5),
                Activity::Running(minute),
                Activity::Running(WARM_UP),
                Activity::Stopped,
            ]),
            Agents {
                total: 4,
                warming: 2,
                settled: 1,
                newest: Some(minute),
            }
        );
        assert_eq!(Agents::count([Activity::Stopped]).newest, None);
    }

    #[test]
    fn an_idle_machine_starts_agents_until_warming_ones_fill_the_target() {
        let settings = Overload::default();
        // Sixteen cores, two assumed per agent: 12.5% each below an 80% target.
        let mut warming = 0;
        while limit(sample(5.0), agents(warming, 0, 60), &settings) > warming {
            warming += 1;
        }
        assert_eq!(warming, 5);
        // Starts are staggered even with room.
        assert_eq!(limit(sample(5.0), agents(1, 0, 1), &settings), 1);
    }

    #[test]
    fn settled_agents_set_the_load_a_new_agent_is_expected_to_add() {
        let settings = Overload::default();
        // Ten mostly idle settled agents at 20%: each counts its 3.125% floor.
        assert_eq!(limit(sample(20.0), agents(0, 10, 600), &settings), 11);
        assert_eq!(limit(sample(20.0), agents(15, 10, 60), &settings), 26);
        assert_eq!(limit(sample(20.0), agents(19, 10, 60), &settings), 29);
        // Two busy settled agents at 60% leave no room for a third at 30%.
        assert_eq!(limit(sample(60.0), agents(0, 2, 600), &settings), 2);
        let mut full = Overload::default();
        full.warning.cpu_used_percent = 95;
        assert_eq!(limit(sample(60.0), agents(0, 2, 600), &full), 3);
    }

    #[test]
    fn pressure_pauses_starts_and_critical_readings_block_even_the_first() {
        let settings = Overload::default();
        let busy = sample(90.0);
        assert_eq!(limit(busy, Agents::default(), &settings), 1);
        assert_eq!(limit(busy, agents(0, 1, 600), &settings), 1);
        let unmeasured = HostSample {
            cpu: None,
            ..sample(0.0)
        };
        assert_eq!(limit(unmeasured, Agents::default(), &settings), 1);
        assert_eq!(limit(unmeasured, agents(0, 1, 600), &settings), 1);
        let warning = HostSample {
            memory: MemoryState::Warning,
            ..sample(0.0)
        };
        assert_eq!(limit(warning, agents(0, 1, 600), &settings), 1);
        let low_disk = HostSample {
            disk_free: Some(settings.warning.disk_free_bytes() - 1),
            ..sample(0.0)
        };
        assert_eq!(limit(low_disk, agents(0, 1, 600), &settings), 1);
        for blocked in [
            HostSample {
                memory: MemoryState::Critical,
                ..sample(0.0)
            },
            HostSample {
                memory: MemoryState::Unavailable,
                ..sample(0.0)
            },
            HostSample {
                disk_free: None,
                ..sample(0.0)
            },
            HostSample {
                disk_free: Some(settings.disk.stop_free_bytes() - 1),
                ..sample(0.0)
            },
        ] {
            assert_eq!(limit(blocked, Agents::default(), &settings), 0);
        }
    }

    #[test]
    fn low_disk_holds_fixed_limits_and_critical_disk_starts_none() {
        let settings = Overload::default();
        let warning = settings.warning.disk_free_bytes();
        let stop = settings.disk.stop_free_bytes();
        assert_eq!(disk_limit(Some(warning), 3, &settings), None);
        assert_eq!(disk_limit(Some(warning - 1), 3, &settings), Some(3));
        assert_eq!(disk_limit(Some(stop), 0, &settings), Some(1));
        assert_eq!(disk_limit(Some(stop - 1), 0, &settings), Some(0));
        assert_eq!(disk_limit(None, 0, &settings), Some(0));
    }
}
