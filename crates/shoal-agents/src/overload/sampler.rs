//! The overload monitor's sampling thread. It reads the machine and decides
//! on an OS thread of its own, so a stalled async runtime delays neither, and
//! hands warnings and stops to the async side without waiting on them.
use std::{
    sync::{Arc, mpsc as std_mpsc},
    thread::JoinHandle,
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};

use super::{
    Monitor, Pressure, Recovery, pressure_log::PressureLog, protection_changed, publish_busy_cpu,
};
use crate::{
    Agents,
    config::{Config, MachineLimit},
    daemon::Daemon,
    log,
    process::load::{self, MemoryReading},
    warnings::{Signal, Warning},
};

/// One sample's warnings, with when it was taken.
pub(super) type Warned = (Instant, Vec<(Signal, Warning)>);

/// The async side's ends of the handoff from the sampling thread.
pub(super) struct Handoff {
    pub(super) warnings: mpsc::Receiver<Warned>,
    /// The pressure to stop work for, one stop at a time.
    pub(super) stops: mpsc::Receiver<Pressure>,
    /// The pressure the latest sample found, which a stop handed over
    /// earlier must still find before it stops anything.
    pub(super) pressing: watch::Receiver<Option<Pressure>>,
    /// Whether each stop stopped anything.
    pub(super) outcomes: std_mpsc::Sender<bool>,
}

/// Start the sampling thread on the machine's clock.
pub(super) fn spawn(
    agents: Arc<Agents>,
    daemon: Arc<dyn Daemon>,
) -> std::io::Result<(JoinHandle<()>, Handoff)> {
    let (sampler, handoff) = Sampler::new(agents, daemon);
    let thread = std::thread::Builder::new()
        .name("overload monitor".to_owned())
        .spawn(move || sampler.run(MachineClock))?;
    Ok((thread, handoff))
}

/// The time the sampling thread reads and waits on.
pub(super) trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&mut self, duration: Duration);
}

struct MachineClock;

impl Clock for MachineClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub(super) struct Sampler {
    agents: Arc<Agents>,
    daemon: Arc<dyn Daemon>,
    /// The config the monitor's windows were observed under.
    config: Arc<Config>,
    monitor: Monitor,
    pressure_log: PressureLog,
    warnings: mpsc::Sender<Warned>,
    stops: mpsc::Sender<Pressure>,
    pressing: watch::Sender<Option<Pressure>>,
    outcomes: std_mpsc::Receiver<bool>,
    /// A stop handed over awaits its outcome, so no other is handed over.
    stopping: bool,
}

impl Sampler {
    pub(super) fn new(agents: Arc<Agents>, daemon: Arc<dyn Daemon>) -> (Self, Handoff) {
        // One sample's warnings at a time: the async side warns each space
        // at most once per repeat interval, so one it has no room for loses
        // nothing a later sample does not bring.
        let (warnings, warnings_out) = mpsc::channel(1);
        let (stops, stops_out) = mpsc::channel(1);
        let (pressing, pressing_out) = watch::channel(None);
        let (outcomes_in, outcomes) = std_mpsc::channel();
        let sampler = Self {
            config: agents.config.current(),
            agents,
            daemon,
            monitor: Monitor::default(),
            pressure_log: PressureLog::default(),
            warnings,
            stops,
            pressing,
            outcomes,
            stopping: false,
        };
        let handoff = Handoff {
            warnings: warnings_out,
            stops: stops_out,
            pressing: pressing_out,
            outcomes: outcomes_in,
        };
        (sampler, handoff)
    }

    /// Sample each poll interval on `clock` until the async side has gone.
    /// Recovery is published with every protection off too: an agent
    /// stopped before they were disabled still waits for it.
    pub(super) fn run(mut self, mut clock: impl Clock) {
        let mut wait = Duration::from_secs(self.config.machine.overload.poll_seconds);
        loop {
            clock.sleep(wait);
            let Some(next) = self.sample(clock.now()) else {
                return;
            };
            wait = next;
        }
    }

    /// Take one sample at `now` and act on it, returning how long to wait
    /// for the next; `None` once the async side has gone.
    pub(super) fn sample(&mut self, now: Instant) -> Option<Duration> {
        if self.stops.is_closed() {
            return None;
        }
        if self.stopped() {
            self.pressing.send_replace(None);
            let cooldown = self.config.machine.overload.cooldown_seconds;
            return Some(Duration::from_secs(cooldown));
        }
        // Check after sleeping, so a reload during it cannot be sampled
        // under the old thresholds. Publication already withdrew readiness,
        // and earlier readings do not count toward the new thresholds.
        let current = self.agents.config.current();
        let changed = protection_changed(&self.config, &current);
        self.config = current;
        if changed {
            self.monitor = Monitor::default();
            self.pressing.send_replace(None);
        } else {
            let readings = Readings::take(&self.config, &self.agents, &*self.daemon);
            self.decide(now, readings);
        }
        Some(Duration::from_secs(
            self.config.machine.overload.poll_seconds,
        ))
    }

    /// Whether a stop handed over has since stopped work, which restarts
    /// every window.
    fn stopped(&mut self) -> bool {
        let Ok(stopped) = self.outcomes.try_recv() else {
            return false;
        };
        self.stopping = false;
        if stopped {
            self.monitor = Monitor::default();
        }
        stopped
    }

    /// Act on `readings` taken at `now`: publish what admission and
    /// recovery read, and hand over this sample's warnings and a stop.
    pub(super) fn decide(&mut self, now: Instant, readings: Readings) {
        let config = self.config.clone();
        let settings = &config.machine.overload;
        if let Some(reading) = &readings.memory {
            self.pressure_log.observe(now, reading);
        }
        let memory = readings.memory.map(|reading| reading.pressure);
        let admission = admission(&config);
        publish_busy_cpu(&self.agents, now, settings, admission, &readings.cpu);
        let pressure = self.monitor.reason(now, settings, memory, readings.cpu);
        self.publish_recovery(now, settings, memory, readings.disk_safe);
        let warnings = self
            .monitor
            .warnings(now, settings, memory, readings.limited);
        self.hand_over(now, warnings, pressure);
    }

    /// Advance the healthy window on this sample's readings and publish it.
    fn publish_recovery(
        &mut self,
        now: Instant,
        settings: &crate::config::overload::Overload,
        memory: Option<load::MemoryPressure>,
        disk_safe: bool,
    ) {
        let recovery = &self.agents.overload_recovery;
        let epoch = recovery.epoch();
        match self
            .monitor
            .recovery(now, settings, epoch, memory, disk_safe)
        {
            Recovery::Revoke => recovery.reset(),
            Recovery::Publish(ready) => recovery.publish(ready),
        }
    }

    /// Hand the async side this sample's `warnings` and `pressure`, and a
    /// stop for it unless one awaits its outcome.
    fn hand_over(
        &mut self,
        now: Instant,
        warnings: Vec<(Signal, Warning)>,
        pressure: Option<Pressure>,
    ) {
        if !warnings.is_empty() {
            let _ = self.warnings.try_send((now, warnings));
        }
        self.pressing.send_replace(pressure);
        if let Some(pressure) = pressure
            && !self.stopping
        {
            self.stopping = self.stops.try_send(pressure).is_ok();
        }
    }
}

/// Whether job admission reads CPU ticks under `config`.
fn admission(config: &Config) -> bool {
    config.machine.max_jobs == MachineLimit::Dynamic
}

/// One sample's machine readings. A disabled protection reads as calm.
pub(super) struct Readings {
    /// `None` with memory protection off or a failed reading.
    pub(super) memory: Option<MemoryReading>,
    /// `memory` counts Shoal's work against its memory limit.
    pub(super) limited: bool,
    pub(super) cpu: anyhow::Result<load::CpuTicks>,
    pub(super) disk_safe: bool,
}

impl Readings {
    /// Take one sample, reading CPU ticks for CPU protection or dynamic job
    /// admission.
    fn take(config: &Config, agents: &Agents, daemon: &dyn Daemon) -> Self {
        let settings = &config.machine.overload;
        let work = daemon.work_limit();
        Self {
            memory: memory_reading(settings, work),
            limited: work.is_some(),
            cpu: if settings.cpu.enabled || admission(config) {
                load::cpu_ticks()
            } else {
                Ok(load::CpuTicks::default())
            },
            disk_safe: agents.config.disk_space_recovered(),
        }
    }
}

/// This sample's memory reading against the configured levels, the
/// machine's with that of Shoal's work against its `work` limit, `None` with
/// memory protection off or a failed reading, which is logged.
fn memory_reading(
    settings: &crate::config::overload::Overload,
    work: Option<load::WorkLimit>,
) -> Option<MemoryReading> {
    if !settings.memory.enabled {
        return None;
    }
    load::memory(&settings.memory_levels(), work)
        .inspect_err(|error| log!("memory overload monitor: {error:#}"))
        .ok()
}

#[cfg(test)]
mod tests;
