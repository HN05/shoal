//! Stop one connected agent after sustained machine pressure, then let the
//! machine recover before evaluating another. Work and leases stay owned.
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;

use super::workspace::Manager;

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
    healthy: Sustained,
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
            self.healthy = Sustained::default();
            self.previous_cpu = None;
        }
        let memory = memory.unwrap_or_else(|error| {
            eprintln!("memory overload monitor: {error:#}");
            false
        });
        let used = match cpu {
            Ok(ticks) => self
                .previous_cpu
                .replace(ticks)
                .and_then(|previous| ticks.used_percent_since(previous)),
            Err(error) => {
                eprintln!("CPU overload monitor: {error:#}");
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
}

pub(super) async fn run(manager: Arc<Manager>) {
    let mut monitor = Monitor::default();
    loop {
        // Read the settings each round so a config reload applies here too.
        let config = manager.config();
        let settings = &config.overload;
        tokio::time::sleep(Duration::from_secs(settings.poll_seconds)).await;
        if !settings.memory.enabled && !settings.cpu.enabled {
            monitor = Monitor::default();
            continue;
        }
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
        let epoch = manager
            .recovery_epoch
            .load(std::sync::atomic::Ordering::Relaxed);
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
        let recovered = monitor.healthy.observe(
            now,
            memory_safe && cpu_safe,
            settings.recovery.sustained_seconds,
        );
        manager
            .recovery_ready
            .send_replace(recovered.then_some(epoch));
        if let Some(reason) = reason
            && manager.stop_agent_for_overload(reason).await
        {
            monitor = Monitor::default();
            tokio::time::sleep(Duration::from_secs(settings.cooldown_seconds)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_readings_reset_pressure_and_default_memory_is_immediate() {
        let start = Instant::now();
        let settings = crate::config::overload::Overload::default();
        let mut monitor = Monitor::default();
        let ticks = crate::process::load::CpuTicks::default();
        assert_eq!(
            monitor.reason(start, &settings, Ok(true), Ok(ticks)),
            Some("critical memory pressure")
        );
        assert_eq!(
            monitor.reason(
                start,
                &settings,
                Err(anyhow::anyhow!("unavailable")),
                Ok(ticks)
            ),
            None
        );
        assert!(monitor.memory.since.is_none());
    }

    #[test]
    fn cpu_is_opt_in_and_requires_continuous_samples() {
        let start = Instant::now();
        let mut settings = crate::config::overload::Overload {
            poll_seconds: 100,
            ..Default::default()
        };
        let mut monitor = Monitor::default();
        let ticks = |n: u64| {
            Ok(crate::process::load::CpuTicks {
                busy: 95 * n,
                total: 100 * n,
            })
        };
        assert_eq!(monitor.reason(start, &settings, Ok(false), ticks(0)), None);
        assert_eq!(
            monitor.reason(
                start + Duration::from_secs(600),
                &settings,
                Ok(false),
                ticks(1)
            ),
            None
        );
        settings.cpu.enabled = true;
        assert_eq!(
            monitor.reason(
                start + Duration::from_secs(601),
                &settings,
                Ok(false),
                ticks(2)
            ),
            None
        );
        assert_eq!(
            monitor.reason(
                start + Duration::from_secs(900),
                &settings,
                Ok(false),
                ticks(3)
            ),
            None
        );
        assert_eq!(
            monitor.reason(
                start + Duration::from_secs(901),
                &settings,
                Ok(false),
                ticks(4)
            ),
            Some("sustained CPU overload")
        );
        assert_eq!(
            monitor.reason(
                start + Duration::from_secs(902),
                &settings,
                Ok(false),
                Err(anyhow::anyhow!("unavailable"))
            ),
            None
        );
        assert!(monitor.cpu.since.is_none());
        assert_eq!(
            monitor.reason(
                start + Duration::from_secs(903),
                &settings,
                Ok(true),
                ticks(5)
            ),
            Some("critical memory pressure")
        );
    }

    #[test]
    fn pressure_must_be_continuous_and_restart_after_recovery() {
        let start = Instant::now();
        let mut pressure = Sustained::default();
        assert!(!pressure.observe(start, true, 30));
        assert!(!pressure.observe(start + Duration::from_secs(29), true, 30));
        assert!(!pressure.observe(start + Duration::from_secs(30), false, 30));
        assert!(!pressure.observe(start + Duration::from_secs(31), true, 30));
        assert!(!pressure.observe(start + Duration::from_secs(60), true, 30));
        assert!(pressure.observe(start + Duration::from_secs(61), true, 30));
    }
}
