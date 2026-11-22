use crate::daemon::overload::*;
use std::time::Duration;
use tokio::time::Instant;

/// A hang guard on the paused clock, well past any recovery window.
async fn wait_published(ready: &mut tokio::sync::watch::Receiver<Option<u64>>) {
    tokio::time::timeout(Duration::from_secs(3600), ready.wait_for(Option::is_some))
        .await
        .expect("recovery was never published")
        .unwrap();
}

#[tokio::test]
async fn recovery_progresses_after_every_protection_is_disabled() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    crate::daemon::test_support::configure(
        &manager,
        "[machine.overload.memory]\nenabled = false\n[machine.overload.disk]\nenabled = false\n",
    )
    .await;
    let mut ready = manager.agents.overload_recovery().watch();
    tokio::time::pause();
    let monitor = tokio::spawn(run(manager.agents.clone(), manager.clone()));
    wait_published(&mut ready).await;
    let epoch = manager.agents.overload_recovery().epoch();
    assert_eq!(*ready.borrow(), Some(epoch));
    monitor.abort();
}

#[tokio::test]
async fn recovery_waits_for_free_disk_space() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let mut config = crate::config::Config::default();
    config.machine.overload.memory.enabled = false;
    manager.publish_config(config);
    let mut ready = manager.agents.overload_recovery().watch();
    tokio::time::pause();
    let monitor = tokio::spawn(run(manager.agents.clone(), manager.clone()));
    tokio::time::sleep(Duration::from_secs(600)).await;
    assert_eq!(*ready.borrow(), None);
    let generation = manager.config_state.generation();
    manager.config_state.publish_disk_reading(Some(generation));
    let recovered = Instant::now();
    wait_published(&mut ready).await;
    let window = Duration::from_secs(manager.config().machine.overload.recovery.sustained_seconds);
    assert!(recovered.elapsed() >= window);
    monitor.abort();
}

#[tokio::test]
async fn unsafe_readings_revoke_the_recovery_epoch() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let mut config = crate::config::Config::default();
    config.machine.overload.memory.enabled = false;
    manager.publish_config(config);
    let generation = manager.config_state.generation();
    manager.config_state.publish_disk_reading(Some(generation));
    let mut ready = manager.agents.overload_recovery().watch();
    tokio::time::pause();
    let monitor = tokio::spawn(run(manager.agents.clone(), manager.clone()));
    wait_published(&mut ready).await;
    let epoch = || manager.agents.overload_recovery().epoch();
    // Admitting a restore advances the epoch it then holds while it checks
    // its workspace.
    manager.agents.overload_recovery().reset();
    let admitted = epoch();
    manager.config_state.publish_disk_reading(None);
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
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let config = |sustained_seconds| {
        let mut config = crate::config::Config::default();
        config.machine.overload.memory.enabled = false;
        config.machine.overload.disk.enabled = false;
        config.machine.overload.recovery.sustained_seconds = sustained_seconds;
        config
    };
    manager.publish_config(config(60));
    let mut ready = manager.agents.overload_recovery().watch();
    tokio::time::pause();
    let start = Instant::now();
    let monitor = tokio::spawn(run(manager.agents.clone(), manager.clone()));
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
async fn job_admission_reads_the_cpu_only_while_it_applies() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let settings = crate::config::overload::Overload::default();
    let window = Duration::from_secs(settings.warning.cpu_sustained_seconds);
    let start = Instant::now();
    let ticks = |n: u64| {
        Ok(crate::process::load::CpuTicks {
            used: 40 * n,
            niced: 50 * n,
            total: 100 * n,
        })
    };
    let publish = |n: u32, admission| {
        let at = start + Duration::from_secs(settings.poll_seconds) * n;
        publish_busy_cpu(&manager.agents, at, &settings, admission, &ticks(n.into()));
    };
    let polls = u32::try_from(window.as_secs() / settings.poll_seconds).unwrap();
    for n in 0..=polls {
        publish(n, true);
    }
    let gap = sample_gap(&settings);
    let percent = |n: u32| {
        let at = start + Duration::from_secs(settings.poll_seconds) * n;
        manager.agents.busy_cpu().percent(at, window, gap)
    };
    // Niced time counts as busy here, unlike to overload protection.
    assert_eq!(percent(polls), Some(90.0));
    publish(polls + 1, false);
    assert_eq!(percent(polls + 1), None);
    let failed = Err(anyhow::anyhow!("unreadable"));
    for n in 0..=polls {
        publish(n, true);
    }
    publish_busy_cpu(
        &manager.agents,
        start + window * 2,
        &settings,
        true,
        &failed,
    );
    assert_eq!(
        manager
            .agents
            .busy_cpu()
            .percent(start + window * 2, window, gap),
        None
    );
}

mod stops;

/// Publish a calm config, then `tighten` it once recovery is ready: the
/// reload withdraws readiness until a new window has passed.
async fn a_reload_withdraws_readiness(tighten: fn(&mut crate::config::Config)) {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let calm = || {
        let mut config = crate::config::Config::default();
        config.machine.overload.memory.enabled = false;
        config.machine.overload.disk.enabled = false;
        config
    };
    let mut tightened = calm();
    tighten(&mut tightened);
    manager.publish_config(calm());
    let mut ready = manager.agents.overload_recovery().watch();
    tokio::time::pause();
    let monitor = tokio::spawn(run(manager.agents.clone(), manager.clone()));
    wait_published(&mut ready).await;
    // Tightening withdraws readiness before any agent can use it.
    manager.publish_config(tightened);
    assert_eq!(*ready.borrow_and_update(), None);
    let reloaded = Instant::now();
    wait_published(&mut ready).await;
    let window = Duration::from_secs(manager.config().machine.overload.recovery.sustained_seconds);
    assert!(reloaded.elapsed() >= window);
    monitor.abort();
}

#[tokio::test]
async fn a_reload_of_a_threshold_withdraws_published_readiness_until_a_new_window() {
    a_reload_withdraws_readiness(|config| {
        config.machine.overload.recovery.memory_used_percent = 75;
    })
    .await;
}

#[tokio::test]
async fn a_reload_of_the_memory_limit_withdraws_published_readiness_until_a_new_window() {
    a_reload_withdraws_readiness(|config| {
        config.machine.memory_limit =
            Some(crate::config::memory_limit::MemoryLimit::Bytes(8 << 30));
    })
    .await;
}
