//! Dynamic agent capacity from the daemon's existing host measurements.
//!
//! Admission deliberately uses this small provider boundary instead of
//! embedding platform probes in queue ordering. New resource signals can be
//! added to [`HostSample`] without changing the queue's accounting rules.
use std::path::Path;

use crate::{config::overload::Overload, fsutil, process::load};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HostSample {
    pub parallelism: u32,
    pub cpu_ticks: Option<load::CpuTicks>,
    pub memory: MemoryState,
    pub disk_free: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryState {
    Healthy,
    Warning,
    Critical,
    Unavailable,
}

/// Probe the same memory and disk sources used by overload protection. A
/// failed probe is retained as `Unavailable`; it never authorizes more work.
pub(crate) fn sample(paths: &[&Path], settings: &Overload) -> HostSample {
    HostSample {
        parallelism: std::thread::available_parallelism()
            .map(|value| value.get().min(u32::MAX as usize) as u32)
            .unwrap_or(1)
            .max(1),
        cpu_ticks: load::cpu_ticks().ok(),
        memory: sample_memory(settings),
        disk_free: sample_disk(paths, settings.disk.enabled, fsutil::available_bytes),
    }
}

fn sample_memory(settings: &Overload) -> MemoryState {
    if !settings.memory.enabled {
        MemoryState::Healthy
    } else {
        match load::critical_memory(settings.memory.used_percent) {
            Ok(true) => MemoryState::Critical,
            Ok(false) if settings.warning.enabled => {
                match load::memory_warning(settings.warning.memory_used_percent) {
                    Ok(true) => MemoryState::Warning,
                    Ok(false) => MemoryState::Healthy,
                    Err(_) => MemoryState::Unavailable,
                }
            }
            Ok(false) => MemoryState::Healthy,
            Err(_) => MemoryState::Unavailable,
        }
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
        .try_fold(None::<u64>, |minimum, path| {
            let free = available(path).ok()?;
            Some(Some(minimum.map_or(free, |current| current.min(free))))
        })
        .flatten()
}

/// Convert a sample into an admission limit. Pressure reserves capacity for
/// cleanup and recovery; future resource policies can extend this reducer.
pub(crate) fn limit(sample: HostSample, settings: &Overload) -> u32 {
    if matches!(
        sample.memory,
        MemoryState::Critical | MemoryState::Unavailable
    ) || sample.disk_free.is_none()
        || sample
            .disk_free
            .is_some_and(|free| free < settings.disk.stop_free_bytes())
    {
        return 0;
    }
    let mut limit = sample.parallelism;
    if sample.memory == MemoryState::Warning
        || sample.disk_free.is_some_and(|free| {
            settings.warning.enabled && free < settings.warning.disk_free_bytes()
        })
    {
        limit = limit.div_ceil(2).max(1);
    }
    limit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_settings() -> Overload {
        Overload::default()
    }

    fn sample(memory: MemoryState, disk_free: Option<u64>) -> HostSample {
        HostSample {
            parallelism: 8,
            cpu_ticks: None,
            memory,
            disk_free,
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
    }

    #[test]
    fn pressure_reduces_capacity_without_changing_slot_accounting() {
        let settings = default_settings();
        let ample = settings.warning.disk_free_bytes() + 1;
        assert_eq!(
            limit(sample(MemoryState::Healthy, Some(ample)), &settings),
            8
        );
        assert_eq!(
            limit(sample(MemoryState::Warning, Some(ample)), &settings),
            4
        );
        assert_eq!(
            limit(sample(MemoryState::Critical, Some(ample)), &settings),
            0
        );
        assert_eq!(limit(sample(MemoryState::Healthy, None), &settings), 0);
        assert_eq!(
            limit(
                sample(
                    MemoryState::Healthy,
                    Some(settings.warning.disk_free_bytes() - 1)
                ),
                &settings
            ),
            4
        );
        let warning_free = settings.warning.disk_free_bytes();
        let stop_free = settings.disk.stop_free_bytes();
        let mut warnings_disabled = default_settings();
        warnings_disabled.warning.enabled = false;
        assert_eq!(
            limit(
                sample(MemoryState::Healthy, Some(warning_free - 1),),
                &warnings_disabled,
            ),
            8
        );
        assert_eq!(
            limit(sample(MemoryState::Healthy, Some(stop_free - 1)), &settings),
            0
        );
    }
}
