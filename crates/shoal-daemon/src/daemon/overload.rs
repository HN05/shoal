//! Stop work after sustained machine pressure, one entry of the stop order
//! at a time, then let the machine recover before evaluating another. Work
//! and leases stay owned.
mod stops;

use std::{sync::Arc, time::Duration};
use tokio::time::Instant;

use crate::process::load::{self, MemoryPressure};

use super::{
    manager::Manager,
    warnings::{self, Signal, Warnings},
};
use crate::daemon::log;

/// What a sample found the machine short of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    Memory,
    Cpu,
}

impl Pressure {
    /// Why protection stops work for it.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Memory => "critical memory pressure",
            Self::Cpu => "sustained CPU overload",
        }
    }
}

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

/// One sample's machine readings. A disabled protection reads as calm.
struct Readings {
    /// `None` with memory protection off or a failed reading.
    memory: Option<MemoryPressure>,
    /// `memory` counts Shoal's work against its memory limit.
    limited: bool,
    cpu: anyhow::Result<load::CpuTicks>,
    disk_safe: bool,
}

impl Readings {
    /// Take one sample, reading CPU ticks when `cpu`: for CPU protection or
    /// dynamic job admission.
    fn take(settings: &crate::config::overload::Overload, cpu: bool, manager: &Manager) -> Self {
        let work = manager.work_limit();
        Self {
            memory: memory_pressure(settings, work),
            limited: work.is_some(),
            cpu: if cpu {
                load::cpu_ticks()
            } else {
                Ok(load::CpuTicks::default())
            },
            disk_safe: manager.config_state.disk_space_recovered(),
        }
    }
}

/// This sample's memory pressure against the configured levels, the
/// machine's with that of Shoal's work against its `work` limit, `None` with
/// memory protection off or a failed reading, which is logged.
fn memory_pressure(
    settings: &crate::config::overload::Overload,
    work: Option<load::WorkLimit>,
) -> Option<MemoryPressure> {
    if !settings.memory.enabled {
        return None;
    }
    load::memory(&settings.memory_levels(), work)
        .map(|reading| reading.pressure)
        .inspect_err(|error| log!("memory overload monitor: {error:#}"))
        .ok()
}

/// What one sample means for the published recovery.
#[derive(Debug, PartialEq)]
enum Recovery {
    /// Readings that turned unsafe revoke a restore admitted on the earlier
    /// ones before its agent starts.
    Revoke,
    /// The epoch the readings were sustained healthy in, if they were.
    Publish(Option<u64>),
}

#[derive(Default)]
struct Monitor {
    memory_stall: Sustained,
    cpu: Sustained,
    cpu_warning: Sustained,
    healthy: Sustained,
    /// Whether the last sample was safe for recovery, across window resets.
    safe: bool,
    recovery_epoch: u64,
    cpu_used: Option<f64>,
    previous_cpu: Option<load::CpuTicks>,
    last_observation: Option<Instant>,
}

impl Monitor {
    fn reason(
        &mut self,
        now: Instant,
        settings: &crate::config::overload::Overload,
        memory: Option<MemoryPressure>,
        cpu: anyhow::Result<load::CpuTicks>,
    ) -> Option<Pressure> {
        if self
            .last_observation
            .replace(now)
            .is_some_and(|previous| now.duration_since(previous) > sample_gap(settings))
        {
            self.memory_stall = Sustained::default();
            self.cpu = Sustained::default();
            self.cpu_warning = Sustained::default();
            self.healthy = Sustained::default();
            self.previous_cpu = None;
        }
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
        // Memory in use stops at once; stall only once it has held.
        let memory = memory.filter(|_| settings.memory.enabled);
        let stalled = self.memory_stall.observe(
            now,
            memory.is_some_and(|memory| memory.stalled),
            settings.memory.sustained_seconds,
        );
        let memory = stalled || memory.is_some_and(|memory| memory.critical);
        let cpu = self.cpu.observe(
            now,
            settings.cpu.enabled
                && used.is_some_and(|used| used >= f64::from(settings.cpu.used_percent)),
            settings.cpu.sustained_seconds,
        );
        if memory {
            Some(Pressure::Memory)
        } else if cpu {
            Some(Pressure::Cpu)
        } else {
            None
        }
    }

    /// Signals past their warning thresholds, after [`Self::reason`] has
    /// taken this sample's CPU reading. Each warns only while its protection is
    /// enabled; a failed reading warns about nothing. `limited` says that
    /// `memory` counts Shoal's work against its memory limit.
    fn warnings(
        &mut self,
        now: Instant,
        settings: &crate::config::overload::Overload,
        memory: Option<MemoryPressure>,
        limited: bool,
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
        let memory =
            enabled(settings.memory.enabled) && memory.is_some_and(|memory| memory.warning);
        let mut signals = Vec::new();
        if memory {
            signals.push((Signal::Memory, warnings::memory(settings, limited)));
        }
        if cpu {
            signals.push((Signal::Cpu, warnings::cpu(settings)));
        }
        signals
    }

    /// Advance the healthy window in the current recovery `epoch`, after
    /// [`Self::reason`] has taken this sample's CPU reading. A new epoch
    /// restarts the window; a failed memory reading is unsafe.
    fn recovery(
        &mut self,
        now: Instant,
        settings: &crate::config::overload::Overload,
        epoch: u64,
        memory: Option<MemoryPressure>,
        disk_safe: bool,
    ) -> Recovery {
        if self.recovery_epoch != epoch {
            self.healthy = Sustained::default();
            self.recovery_epoch = epoch;
        }
        let recovery = &settings.recovery;
        let memory_safe = !settings.memory.enabled || memory.is_some_and(|memory| memory.safe);
        let cpu_safe = !settings.cpu.enabled
            || self
                .cpu_used
                .is_some_and(|used| used < f64::from(recovery.cpu_used_percent));
        let disk_safe = !settings.disk.enabled || disk_safe;
        let safe = memory_safe && cpu_safe && disk_safe;
        let was_safe = std::mem::replace(&mut self.safe, safe);
        let recovered = self.healthy.observe(now, safe, recovery.sustained_seconds);
        if was_safe && !safe {
            Recovery::Revoke
        } else {
            Recovery::Publish(recovered.then_some(epoch))
        }
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
        tokio::time::sleep(Duration::from_secs(config.machine.overload.poll_seconds)).await;
        let operation = manager.gates.operation().await;
        // Check after sleeping, so a reload during it cannot be sampled under
        // the old thresholds. Publication already withdrew readiness, and
        // earlier readings do not count toward the new thresholds.
        let current = manager.config();
        let changed = protection_changed(&config, &current);
        config = current;
        if changed {
            monitor = Monitor::default();
            continue;
        }
        let settings = &config.machine.overload;
        let admission = config.machine.max_jobs == crate::config::MachineLimit::Dynamic;
        let readings = Readings::take(settings, settings.cpu.enabled || admission, &manager);
        let now = Instant::now();
        publish_busy_cpu(&manager, now, settings, admission, &readings.cpu);
        let pressure = monitor.reason(now, settings, readings.memory, readings.cpu);
        // Decide before awaiting warnings, so a reload meanwhile cannot pair
        // these readings with its new epoch.
        let epoch = manager.overload_recovery.epoch();
        match monitor.recovery(now, settings, epoch, readings.memory, readings.disk_safe) {
            Recovery::Revoke => manager.overload_recovery.reset(),
            Recovery::Publish(ready) => manager.overload_recovery.publish(ready),
        }
        for (signal, message) in monitor.warnings(now, settings, readings.memory, readings.limited)
        {
            warned.warn(&manager, now, signal, &message).await;
        }
        if let Some(pressure) = pressure
            && manager.stop_for_overload(pressure).await
        {
            monitor = Monitor::default();
            drop(operation);
            tokio::time::sleep(Duration::from_secs(settings.cooldown_seconds)).await;
        }
    }
}

/// Whether `new` measures load against other levels than `old`: its overload
/// settings or the memory limit on Shoal's work changed.
pub(super) fn protection_changed(old: &crate::config::Config, new: &crate::config::Config) -> bool {
    old.machine.overload != new.machine.overload
        || old.machine.memory_limit != new.machine.memory_limit
}

/// Give job admission this sample's CPU ticks while `admission` reads
/// them, over the CPU warning's window; a failed reading leaves it none.
fn publish_busy_cpu(
    manager: &Manager,
    now: Instant,
    settings: &crate::config::overload::Overload,
    admission: bool,
    cpu: &anyhow::Result<load::CpuTicks>,
) {
    manager.busy_cpu.observe(
        now,
        cpu.as_ref().ok().copied().filter(|_| admission),
        sample_gap(settings),
        Duration::from_secs(settings.warning.cpu_sustained_seconds),
    );
}

/// The longest one sample may follow another before the readings no longer
/// count as continuous.
pub(super) fn sample_gap(settings: &crate::config::overload::Overload) -> Duration {
    Duration::from_secs(settings.poll_seconds * 3)
}

#[cfg(test)]
mod tests;
