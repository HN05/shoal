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
pub(crate) fn sample(state_path: &Path, settings: &Overload) -> HostSample {
    let memory = if !settings.memory.enabled {
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
    };
    HostSample {
        parallelism: std::thread::available_parallelism()
            .map(|value| value.get().min(u32::MAX as usize) as u32)
            .unwrap_or(1)
            .max(1),
        cpu_ticks: load::cpu_ticks().ok(),
        memory,
        disk_free: settings
            .disk
            .enabled
            .then(|| fsutil::available_bytes(state_path).ok())
            .flatten()
            .or_else(|| (!settings.disk.enabled).then_some(u64::MAX)),
    }
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
        || sample
            .disk_free
            .is_some_and(|free| free < settings.disk.cleanup_free_bytes())
    {
        limit = limit.div_ceil(2).max(1);
    }
    limit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Overload {
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
    fn pressure_reduces_capacity_without_changing_slot_accounting() {
        let settings = settings();
        let ample = settings.disk.cleanup_free_bytes() + 1;
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
                    Some(settings.disk.cleanup_free_bytes() - 1)
                ),
                &settings
            ),
            4
        );
        assert_eq!(
            limit(
                sample(
                    MemoryState::Healthy,
                    Some(settings.disk.stop_free_bytes() - 1)
                ),
                &settings
            ),
            0
        );
    }
}
