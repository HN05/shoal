//! Stop one connected agent after sustained machine pressure, then let the
//! machine recover before evaluating another. Work and leases stay owned.
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;

use super::{
    warnings::{self, Signal, Warnings},
    workspace::Manager,
};
use crate::daemon::log;

#[derive(Default)]
struct Sustained {
    since: Option<Instant>,
}

impl Sustained {
    fn observe(&mut self, now: Instant, critical: bool, seconds: u64) -> bool {
        if !critical {
            self.since = None;
            return false;
        }
        now.duration_since(*self.since.get_or_insert(now)) >= Duration::from_secs(seconds)
    }
}

#[derive(Default)]
struct Monitor {
    memory: Sustained,
    cpu: Sustained,
    cpu_warning: Sustained,
    healthy: Sustained,
    /// Whether the last sample was safe for recovery, across window resets.
    safe: bool,
    recovery_epoch: u64,
    cpu_used: Option<f64>,
    previous_cpu: Option<crate::process::load::CpuTicks>,
    last_observation: Option<Instant>,
}

impl Monitor {
    fn reason(
        &mut self,
        now: Instant,
        settings: &crate::config::overload::Overload,
        memory: anyhow::Result<bool>,
        cpu: anyhow::Result<crate::process::load::CpuTicks>,
    ) -> Option<&'static str> {
        if self.last_observation.replace(now).is_some_and(|previous| {
            now.duration_since(previous) > Duration::from_secs(settings.poll_seconds * 3)
        }) {
            self.memory = Sustained::default();
            self.cpu = Sustained::default();
            self.cpu_warning = Sustained::default();
            self.healthy = Sustained::default();
            self.previous_cpu = None;
        }
        let memory = memory.unwrap_or_else(|error| {
            log!("memory overload monitor: {error:#}");
            false
        });
        let used = match cpu {
            Ok(ticks) => self
                .previous_cpu
                .replace(ticks)
                .and_then(|previous| ticks.used_percent_since(previous)),
            Err(error) => {
                log!("CPU overload monitor: {error:#}");
                self.previous_cpu = None;
                None
            }
        };
        self.cpu_used = used;
        let memory = self.memory.observe(
            now,
            settings.memory.enabled && memory,
            settings.memory.sustained_seconds,
        );
        let cpu = self.cpu.observe(
            now,
            settings.cpu.enabled
                && used.is_some_and(|used| used >= f64::from(settings.cpu.used_percent)),
            settings.cpu.sustained_seconds,
        );
        if memory {
            Some("critical memory pressure")
        } else if cpu {
            Some("sustained CPU overload")
        } else {
            None
        }
    }

    /// Signals past their warning thresholds, after [`Self::reason`] has
    /// taken this sample's CPU reading. Each warns only while its protection is
    /// enabled; a failed reading warns about nothing.
    fn warnings(
        &mut self,
        now: Instant,
        settings: &crate::config::overload::Overload,
        memory: anyhow::Result<bool>,
    ) -> Vec<(Signal, String)> {
        let warning = &settings.warning;
        let enabled = |protection| warning.enabled && protection;
        let cpu = self.cpu_warning.observe(
            now,
            enabled(settings.cpu.enabled)
                && self
                    .cpu_used
                    .is_some_and(|used| used >= f64::from(warning.cpu_used_percent)),
            warning.cpu_sustained_seconds,
        );
        let memory = enabled(settings.memory.enabled) && memory.unwrap_or(false);
        let mut signals = Vec::new();
        if memory {
            signals.push((Signal::Memory, warnings::memory(settings)));
        }
        if cpu {
            signals.push((Signal::Cpu, warnings::cpu(settings)));
        }
        signals
    }
}

pub(super) async fn run(manager: Arc<Manager>) {
    let mut monitor = Monitor::default();
    // Outlives monitor resets, so a stop does not repeat earlier warnings.
    let mut warned = Warnings::default();
    let mut config = manager.config();
    loop {
        // Keep publishing recovery with every protection off: an agent stopped
        // before they were disabled still waits for it.
        tokio::time::sleep(Duration::from_secs(config.overload.poll_seconds)).await;
        let operation = manager.gates.operation().await;
        // Check after sleeping, so a reload during it cannot be sampled under
        // the old thresholds. Publication already withdrew readiness, and
        // earlier readings do not count toward the new thresholds.
        let current = manager.config();
        let changed = current.overload != config.overload;
        config = current;
        if changed {
            monitor = Monitor::default();
            continue;
        }
        let settings = &config.overload;
        let memory = if settings.memory.enabled {
            crate::process::load::critical_memory(settings.memory.used_percent)
        } else {
            Ok(false)
        };
        let cpu = if settings.cpu.enabled {
            crate::process::load::cpu_ticks()
        } else {
            Ok(crate::process::load::CpuTicks::default())
        };
        let now = Instant::now();
        let reason = monitor.reason(now, settings, memory, cpu);
        let memory_warning = if settings.warning.enabled && settings.memory.enabled {
            crate::process::load::memory_warning(settings.warning.memory_used_percent)
        } else {
            Ok(false)
        };
        for (signal, message) in monitor.warnings(now, settings, memory_warning) {
            warned.warn(&manager, now, signal, &message).await;
        }
        let epoch = manager.overload_recovery.epoch();
        if monitor.recovery_epoch != epoch {
            monitor.healthy = Sustained::default();
            monitor.recovery_epoch = epoch;
        }
        let memory_safe = !settings.memory.enabled
            || crate::process::load::safe_memory(settings.recovery.memory_used_percent)
                .unwrap_or(false);
        let cpu_safe = !settings.cpu.enabled
            || monitor
                .cpu_used
                .is_some_and(|used| used < f64::from(settings.recovery.cpu_used_percent));
        let disk_safe = !settings.disk.enabled || manager.config_state.disk_space_recovered();
        let safe = memory_safe && cpu_safe && disk_safe;
        // Readings that turn unsafe revoke a restore admitted on the earlier
        // ones before its agent starts.
        if std::mem::replace(&mut monitor.safe, safe) && !safe {
            manager.overload_recovery.reset();
        }
        let recovered = monitor
            .healthy
            .observe(now, safe, settings.recovery.sustained_seconds);
        manager
            .overload_recovery
            .publish(recovered.then_some(epoch));
        if let Some(reason) = reason
            && manager.stop_agent_for_overload(reason).await
        {
            monitor = Monitor::default();
            drop(operation);
            tokio::time::sleep(Duration::from_secs(settings.cooldown_seconds)).await;
        }
    }
}

#[cfg(test)]
mod tests;
