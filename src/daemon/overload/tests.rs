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
    crate::test_support::configure(
        &manager,
        "[overload.memory]\nenabled = false\n[overload.disk]\nenabled = false\n",
    )
    .await;
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
async fn unsafe_readings_revoke_the_recovery_epoch() {
    let (_root, manager) = crate::test_support::manager().await;
    let mut config = crate::config::Config::default();
    config.overload.memory.enabled = false;
    manager.publish_config(config);
    let generation = manager
        .config_generation
        .load(std::sync::atomic::Ordering::SeqCst);
    manager.publish_disk_reading(Some(generation));
    let mut ready = manager.recovery_ready.subscribe();
    tokio::time::pause();
    let monitor = tokio::spawn(run(manager.clone()));
    wait_published(&mut ready).await;
    let epoch = || {
        manager
            .recovery_epoch
            .load(std::sync::atomic::Ordering::Relaxed)
    };
    // Admitting a restore advances the epoch it then holds while it checks
    // its workspace.
    manager.reset_overload_recovery();
    let admitted = epoch();
    manager.publish_disk_reading(None);
    tokio::time::timeout(Duration::from_secs(3600), async {
        while epoch() == admitted {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .expect("unsafe readings never revoked the epoch");
    assert_eq!(*ready.borrow(), None);
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
