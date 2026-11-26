//! Stop work after sustained machine pressure, one entry of the stop order
//! at a time, then let the machine recover before evaluating another. Work
//! and leases stay owned.
mod pressure_log;
mod sampler;
pub mod stops;

use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};

use crate::process::load::{self, MemoryPressure};

use crate::Agents;
use crate::daemon::Daemon;
use crate::log;
use crate::warnings::{self, Signal, Warning, Warnings};

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
        // A late sample means the daemon was starved, which ends the windows
        // of CPU tick deltas and of health. Stall's window survives it: the
        // kernel's ten-second average stays valid however late it is read,
        // and a thrashing machine is exactly where samples come late.
        if self
            .last_observation
            .replace(now)
            .is_some_and(|previous| now.duration_since(previous) > sample_gap(settings))
        {
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
    ) -> Vec<(Signal, Warning)> {
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

/// Protect the machine: a thread of its own samples and decides, and this
/// task stops work and warns agents as it hands over.
pub async fn run(agents: Arc<Agents>, daemon: Arc<dyn Daemon>) {
    let (thread, handoff) = sampler::spawn(agents.clone(), daemon.clone())
        .expect("start the overload monitor's thread");
    tokio::join!(
        stop_work(
            &agents,
            &*daemon,
            handoff.stops,
            handoff.pressing,
            handoff.outcomes
        ),
        warn_agents(&agents, handoff.warnings),
    );
    // The handoff ends only once the thread has, which it does only by
    // panicking while this task runs; the panic restarts the monitor.
    if let Ok(Err(panic)) = tokio::task::spawn_blocking(move || thread.join()).await {
        std::panic::resume_unwind(panic);
    }
}

/// Stop work for each stop the sampling thread hands over, telling it
/// whether anything stopped. A stop waits for the runtime and the operation
/// gate, so it acts on the pressure the latest sample found, and stops
/// nothing once a sample found none, as after protection was disabled.
async fn stop_work(
    agents: &Agents,
    daemon: &dyn Daemon,
    mut stops: mpsc::Receiver<Pressure>,
    pressing: watch::Receiver<Option<Pressure>>,
    outcomes: std::sync::mpsc::Sender<bool>,
) {
    while stops.recv().await.is_some() {
        let stopped = {
            let _operation = agents.gates.operation().await;
            let pressure = *pressing.borrow();
            match pressure {
                Some(pressure) => agents.stop_for_overload(daemon, pressure).await,
                None => false,
            }
        };
        if outcomes.send(stopped).is_err() {
            return;
        }
    }
}

/// Queue the warnings the sampling thread hands over.
async fn warn_agents(agents: &Agents, mut warnings: mpsc::Receiver<sampler::Warned>) {
    // Outlives monitor resets, so a stop does not repeat earlier warnings.
    let mut warned = Warnings::default();
    while let Some((now, signals)) = warnings.recv().await {
        let _operation = agents.gates.operation().await;
        for (signal, warning) in signals {
            warned.warn(agents, now, signal, &warning).await;
        }
    }
}

/// Whether `new` measures load against other levels than `old`: its overload
/// settings or the memory limit on Shoal's work changed.
pub fn protection_changed(old: &crate::config::Config, new: &crate::config::Config) -> bool {
    old.machine.overload != new.machine.overload
        || old.machine.work_memory_limit() != new.machine.work_memory_limit()
}

/// Give job admission this sample's CPU ticks while `admission` reads
/// them, over the CPU warning's window; a failed reading leaves it none.
pub fn publish_busy_cpu(
    agents: &Agents,
    now: Instant,
    settings: &crate::config::overload::Overload,
    admission: bool,
    cpu: &anyhow::Result<load::CpuTicks>,
) {
    agents.busy_cpu.observe(
        now,
        cpu.as_ref().ok().copied().filter(|_| admission),
        sample_gap(settings),
        Duration::from_secs(settings.warning.cpu_sustained_seconds),
    );
}

/// The longest one sample may follow another before the readings no longer
/// count as continuous.
pub fn sample_gap(settings: &crate::config::overload::Overload) -> Duration {
    Duration::from_secs(settings.poll_seconds * 3)
}

#[cfg(test)]
mod tests;
