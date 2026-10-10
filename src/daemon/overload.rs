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
        let operation = manager.background_operations.read().await;
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
        let disk_safe = !settings.disk.enabled || manager.disk_space_recovered();
        let recovered = monitor.healthy.observe(
            now,
            memory_safe && cpu_safe && disk_safe,
            settings.recovery.sustained_seconds,
        );
        manager
            .recovery_ready
            .send_replace(recovered.then_some(epoch));
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
mod tests {
    use super::*;

    /// A hang guard on the paused clock, well past any recovery window.
    async fn wait_published(ready: &mut tokio::sync::watch::Receiver<Option<u64>>) {
        tokio::time::timeout(Duration::from_secs(3600), ready.wait_for(Option::is_some))
            .await
            .expect("recovery was never published")
            .unwrap();
    }

    #[tokio::test]
    async fn recovery_progresses_after_every_protection_is_disabled() {
        let (_root, manager) = crate::test_support::manager().await;
        let config = crate::config::Config::path(&manager.paths);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            "[overload.memory]\nenabled = false\n[overload.disk]\nenabled = false\n",
        )
        .unwrap();
        manager.reload_config().await.unwrap();
        let mut ready = manager.recovery_ready.subscribe();
        tokio::time::pause();
        let monitor = tokio::spawn(run(manager.clone()));
        wait_published(&mut ready).await;
        let epoch = manager
            .recovery_epoch
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(*ready.borrow(), Some(epoch));
        monitor.abort();
    }

    #[tokio::test]
    async fn recovery_waits_for_free_disk_space() {
        let (_root, manager) = crate::test_support::manager().await;
        let mut config = crate::config::Config::default();
        config.overload.memory.enabled = false;
        manager.publish_config(config);
        let mut ready = manager.recovery_ready.subscribe();
        tokio::time::pause();
        let monitor = tokio::spawn(run(manager.clone()));
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert_eq!(*ready.borrow(), None);
        let generation = manager
            .config_generation
            .load(std::sync::atomic::Ordering::SeqCst);
        manager.publish_disk_reading(Some(generation));
        let recovered = Instant::now();
        wait_published(&mut ready).await;
        let window = Duration::from_secs(manager.config().overload.recovery.sustained_seconds);
        assert!(recovered.elapsed() >= window);
        monitor.abort();
    }

    #[tokio::test]
    async fn changed_thresholds_restart_the_recovery_window() {
        let (_root, manager) = crate::test_support::manager().await;
        let config = |sustained_seconds| {
            let mut config = crate::config::Config::default();
            config.overload.memory.enabled = false;
            config.overload.disk.enabled = false;
            config.overload.recovery.sustained_seconds = sustained_seconds;
            config
        };
        manager.publish_config(config(60));
        let mut ready = manager.recovery_ready.subscribe();
        tokio::time::pause();
        let start = Instant::now();
        let monitor = tokio::spawn(run(manager.clone()));
        // Most of the old window has passed when the threshold changes.
        tokio::time::sleep(Duration::from_secs(41)).await;
        assert_eq!(*ready.borrow(), None);
        manager.publish_config(config(61));
        wait_published(&mut ready).await;
        // Without a reset the old observations publish at about 62 seconds.
        assert!(start.elapsed() >= Duration::from_secs(41 + 61));
        monitor.abort();
    }

    #[tokio::test]
    async fn a_reload_withdraws_published_readiness_until_a_new_window() {
        let (_root, manager) = crate::test_support::manager().await;
        let config = |memory_used_percent| {
            let mut config = crate::config::Config::default();
            config.overload.memory.enabled = false;
            config.overload.disk.enabled = false;
            config.overload.recovery.memory_used_percent = memory_used_percent;
            config
        };
        manager.publish_config(config(85));
        let mut ready = manager.recovery_ready.subscribe();
        tokio::time::pause();
        let monitor = tokio::spawn(run(manager.clone()));
        wait_published(&mut ready).await;
        // Tightening a threshold withdraws readiness before any agent can use it.
        manager.publish_config(config(75));
        assert_eq!(*ready.borrow_and_update(), None);
        let reloaded = Instant::now();
        wait_published(&mut ready).await;
        let window = Duration::from_secs(manager.config().overload.recovery.sustained_seconds);
        assert!(reloaded.elapsed() >= window);
        monitor.abort();
    }

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
    fn warnings_follow_enabled_protections_and_sustained_cpu() {
        let start = Instant::now();
        let mut settings = crate::config::overload::Overload {
            poll_seconds: 100,
            ..Default::default()
        };
        let mut monitor = Monitor::default();
        let signals = |warnings: Vec<(Signal, String)>| {
            warnings
                .into_iter()
                .map(|(signal, _)| signal)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            signals(monitor.warnings(start, &settings, Ok(true))),
            [Signal::Memory]
        );
        assert!(
            monitor
                .warnings(start, &settings, Err(anyhow::anyhow!("unavailable")))
                .is_empty()
        );
        settings.memory.enabled = false;
        assert!(monitor.warnings(start, &settings, Ok(true)).is_empty());
        settings.memory.enabled = true;
        settings.warning.enabled = false;
        assert!(monitor.warnings(start, &settings, Ok(true)).is_empty());
        settings.warning.enabled = true;

        // CPU warns only while its protection is enabled, after the warning
        // threshold has held for its sustained time.
        settings.cpu.enabled = true;
        monitor.cpu_used = Some(85.0);
        assert!(monitor.warnings(start, &settings, Ok(false)).is_empty());
        let sustained = Duration::from_secs(settings.warning.cpu_sustained_seconds);
        assert_eq!(
            signals(monitor.warnings(start + sustained, &settings, Ok(false))),
            [Signal::Cpu]
        );
        monitor.cpu_used = Some(79.0);
        assert!(
            monitor
                .warnings(start + sustained * 2, &settings, Ok(false))
                .is_empty()
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
